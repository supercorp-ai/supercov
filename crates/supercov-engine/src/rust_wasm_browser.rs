//! Rust tests compiled to wasm that `wasm-bindgen-test-runner` runs in a
//! headless browser (`wasm-pack test --headless --chrome`).
//!
//! The runner opens the browser through WebDriver, loads its test page and
//! reads the results off it. The region each instrumented crate keeps lives in
//! the page's wasm memory, and nothing in the page writes it anywhere, so
//! Supercov stands between the runner and the driver: each test's runner is
//! pointed at a proxy (`CHROMEDRIVER_REMOTE`) whose URL names that test's
//! evidence directory. When the runner opens a session the proxy has the
//! browser remember every wasm instance any page creates (a script evaluated
//! before the page's own); when the runner closes it, the proxy first takes
//! each instance's region and writes it to the test's evidence, where the run
//! reads it as it reads a native process's.
//!
//! Chrome only: the remembering script is installed through Chrome's
//! DevTools protocol. A test run in another browser is said to be unmeasured.

use crate::fault_process::Launch;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use base64::Engine as _;
use serde_json::{Value, json};

/// Remember every wasm instance, however the page makes it: wasm-bindgen's
/// browser glue streams the module.
const REGISTRY: &str = r#"(() => {
  if (globalThis.__supercovWasm) return;
  const instances = (globalThis.__supercovWasm = []);
  const Instance = WebAssembly.Instance;
  function Recorded(module, imports) {
    const instance = new Instance(module, imports);
    instances.push(instance);
    return instance;
  }
  Recorded.prototype = Instance.prototype;
  WebAssembly.Instance = Recorded;
  for (const name of ["instantiate", "instantiateStreaming"]) {
    const original = WebAssembly[name];
    if (typeof original !== "function") continue;
    WebAssembly[name] = async function (...args) {
      const result = await original.apply(WebAssembly, args);
      instances.push(result instanceof Instance ? result : result.instance);
      return result;
    };
  }
})();"#;

/// Each instrumented crate's region, whole, as base64.
const TAKE: &str = r#"const out = [];
for (const instance of globalThis.__supercovWasm ?? []) {
  const exports = instance.exports;
  for (const name of Object.keys(exports)) {
    if (!name.startsWith('__supercov_region_') || !exports.memory) continue;
    const packed = BigInt.asUintN(64, BigInt(exports[name]()));
    const at = Number(packed >> 32n), length = Number(packed & 0xffffffffn);
    if (!length) continue;
    const bytes = new Uint8Array(exports.memory.buffer, at, length);
    let text = '';
    for (let index = 0; index < length; index += 0x8000)
      text += String.fromCharCode.apply(null, bytes.subarray(index, Math.min(length, index + 0x8000)));
    out.push([name.slice('__supercov_region_'.length), btoa(text)]);
  }
}
return out;"#;

/// The browser the runner would pick, as `wasm-bindgen-test-runner` picks
/// it: a `*_REMOTE` driver first, then a driver named in the environment,
/// then the first on `PATH`, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Browser {
    /// chromedriver at this path, with these arguments.
    Chrome(PathBuf, Vec<String>),
    /// Another browser, or a remote driver: its tests run unmeasured.
    Other(String),
}

const DRIVERS: [&str; 4] = [
    "geckodriver",
    "safaridriver",
    "chromedriver",
    "msedgedriver",
];

pub fn browser(environment: &[(String, String)]) -> Option<Browser> {
    browser_from(environment, &|name| std::env::var_os(name))
}

/// `browser`, with `ambient` as the process's own variables.
pub(crate) fn browser_from(
    environment: &[(String, String)],
    ambient: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<Browser> {
    let variable = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| ambient(name)?.into_string().ok())
    };
    let arguments = |driver: &str| {
        variable(&format!("{}_ARGS", driver.to_uppercase()))
            .map(|value| value.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default()
    };
    for driver in DRIVERS {
        if variable(&format!("{}_REMOTE", driver.to_uppercase())).is_some() {
            return Some(Browser::Other(format!("a remote {driver}")));
        }
    }
    let pick = |driver: &str, path: PathBuf| {
        if driver == "chromedriver" {
            Browser::Chrome(path, arguments(driver))
        } else {
            Browser::Other(driver.to_owned())
        }
    };
    for driver in DRIVERS {
        if let Some(path) = variable(&driver.to_uppercase()) {
            return Some(pick(driver, PathBuf::from(path)));
        }
    }
    let path = ambient("PATH")?;
    // Never empty: even an empty PATH is one directory.
    std::env::split_paths(&path).find_map(|directory| {
        DRIVERS.iter().find_map(|driver| {
            let candidate = directory
                .join(driver)
                .with_extension(std::env::consts::EXE_EXTENSION);
            candidate.exists().then(|| pick(driver, candidate))
        })
    })
}

/// The proxy in front of one chromedriver, for the whole run.
#[derive(Debug)]
pub struct BrowserReader {
    address: SocketAddr,
    driver: Child,
    stop: Arc<AtomicBool>,
}

impl BrowserReader {
    pub fn start(chromedriver: &Path, arguments: &[String], token: &str) -> Result<Self, String> {
        Self::start_within(chromedriver, arguments, token, Duration::from_secs(20))
    }

    /// `start`, giving the driver `patience` to listen. A driver that never
    /// does is stopped: left running, it outlived the run.
    fn start_within(
        chromedriver: &Path,
        arguments: &[String],
        token: &str,
        patience: Duration,
    ) -> Result<Self, String> {
        let port = crate::rust_worker::free_port()?;
        let listener = crate::rust_worker::local_listener()?;
        let address = listener
            .local_addr()
            .expect("a bound listener has its address");
        listener
            .set_nonblocking(true)
            .expect("a listener can be made not to block");
        let mut driver = Command::new(chromedriver)
            .args(arguments)
            .arg(format!("--port={port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .launch()
            .map_err(|error| format!("could not start {}: {error}", chromedriver.display()))?;
        let mut stop_driver = |error: String| {
            let _ = driver.kill();
            let _ = driver.wait();
            error
        };
        let upstream = SocketAddr::from(([127, 0, 0, 1], port));
        let started = Instant::now();
        // A driver just started listens only after a moment.
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if TcpStream::connect_timeout(&upstream, Duration::from_millis(100)).is_ok() {
                break;
            }
            if started.elapsed() > patience {
                return Err(stop_driver(format!(
                    "{} never listened",
                    chromedriver.display()
                )));
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let token = token.to_owned();
        let spawned = std::thread::Builder::new()
            .name("supercov-browser-proxy".into())
            .spawn(move || {
                // Stopped only once the reader that started it is dropped.
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // An accepted socket keeps the listener's
                            // non-blocking mode on macOS.
                            let _ = stream.set_nonblocking(false);
                            let token = token.clone();
                            std::thread::spawn(move || {
                                let _ = relay(stream, upstream, &token);
                            });
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                }
            });
        spawned.expect("a thread can be started");
        Ok(Self {
            address,
            driver,
            stop,
        })
    }

    /// The driver URL a test's runner is given: its evidence directory, hex
    /// in the path, is where that session's regions are written.
    pub fn remote_url(&self, evidence: &Path) -> String {
        let hex = evidence
            .to_string_lossy()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("http://{}/e/{hex}/", self.address)
    }
}

impl Drop for BrowserReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    stream.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut length = 0;
    let mut chunked = false;
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok()?;
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked {
        read_chunked(stream)?
    } else {
        read_length(stream, length)?
    };
    Some(Request { method, path, body })
}

/// A body of a declared length, read as its bytes arrive: a length no
/// sender meant is never allocated up front.
fn read_length(reader: &mut impl BufRead, length: usize) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(length.min(1 << 20));
    reader.take(length as u64).read_to_end(&mut body).ok()?;
    (body.len() == length).then_some(body)
}

/// A chunked body, decoded: chunk extensions (`5;name=v`) are ignored, and
/// trailers are read through the blank line that ends them.
fn read_chunked(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        if reader.read_line(&mut size).ok()? == 0 {
            return None;
        }
        let size =
            usize::from_str_radix(size.split(';').next().unwrap_or_default().trim(), 16).ok()?;
        if size == 0 {
            loop {
                let mut trailer = String::new();
                if reader.read_line(&mut trailer).ok()? == 0 || trailer.trim().is_empty() {
                    return Some(body);
                }
            }
        }
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).ok()?;
        body.extend_from_slice(&chunk[..size]);
    }
}

/// One request to chromedriver; its status and body, chunked or not.
fn call(upstream: SocketAddr, method: &str, path: &str, body: &[u8]) -> Option<(u16, Vec<u8>)> {
    let stream = TcpStream::connect(upstream).ok()?;
    // A connected socket takes a timeout: only an unconnected one refuses.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    exchange(stream, upstream, method, path, body)
}

/// [`call`] over a stream already open to chromedriver.
fn exchange(
    mut stream: impl Read + Write,
    upstream: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> Option<(u16, Vec<u8>)> {
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {upstream}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(body).ok()?;
    // Read by the response's own length: chromedriver keeps the connection
    // open after answering, whatever the request asked.
    read_response(&mut BufReader::new(stream))
}

/// A response's status and body, framed by its length, by chunks, or by the
/// connection closing.
fn read_response(reader: &mut impl BufRead) -> Option<(u16, Vec<u8>)> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let status = line.split_whitespace().nth(1)?.parse().ok()?;
    let mut length = None;
    let mut chunked = false;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let payload = if chunked {
        read_chunked(reader)?
    } else if let Some(length) = length {
        read_length(reader, length)?
    } else {
        let mut payload = Vec::new();
        reader.read_to_end(&mut payload).ok()?;
        payload
    };
    Some((status, payload))
}

static REGIONS: AtomicU64 = AtomicU64::new(0);

fn relay(stream: TcpStream, upstream: SocketAddr, token: &str) -> Option<()> {
    let mut reader = BufReader::new(stream.try_clone().expect("a socket can be cloned"));
    let request = read_request(&mut reader)?;
    // `/e/<hex evidence directory>/<WebDriver path>`.
    let rest = request.path.strip_prefix("/e/")?;
    let (hex, webdriver) = rest.split_once('/').unwrap_or((rest, ""));
    let evidence = PathBuf::from(
        String::from_utf8(
            (0..hex.len())
                .step_by(2)
                .filter_map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
                .collect(),
        )
        .ok()?,
    );
    let webdriver = format!("/{webdriver}");
    // The runner closes its window rather than its session; either ends
    // what the page can tell.
    let session = webdriver
        .strip_prefix("/session/")
        .map(|rest| rest.strip_suffix("/window").unwrap_or(rest))
        .filter(|id| !id.contains('/'))
        .map(str::to_owned);
    if request.method == "DELETE"
        && let Some(id) = &session
    {
        take_regions(upstream, id, &evidence, token);
    }
    let (status, body) = call(upstream, &request.method, &webdriver, &request.body)?;
    if request.method == "POST"
        && webdriver == "/session"
        && status == 200
        && let Ok(created) = serde_json::from_slice::<Value>(&body)
        // W3C answers with `value.sessionId`; the older protocol the runner
        // speaks, with `sessionId` beside `value`.
        && let Some(id) = created["value"]["sessionId"]
            .as_str()
            .or_else(|| created["sessionId"].as_str())
    {
        let install = json!({
            "cmd": "Page.addScriptToEvaluateOnNewDocument",
            "params": {"source": REGISTRY},
        });
        let _ = call(
            upstream,
            "POST",
            &format!("/session/{id}/goog/cdp/execute"),
            install.to_string().as_bytes(),
        );
    }
    let mut stream = stream;
    let reason = if status == 200 { "OK" } else { "Error" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(&body).ok()?;
    Some(())
}

/// Before the runner closes its session: each region the page's wasm holds,
/// written to the test's evidence under the run's token.
fn take_regions(upstream: SocketAddr, session: &str, evidence: &Path, token: &str) {
    let script = json!({"script": TAKE, "args": []});
    let Some((200, body)) = call(
        upstream,
        "POST",
        &format!("/session/{session}/execute/sync"),
        script.to_string().as_bytes(),
    ) else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return;
    };
    for region in value["value"].as_array().into_iter().flatten() {
        let Some(bytes) = region
            .get(1)
            .and_then(Value::as_str)
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
        else {
            continue;
        };
        if crate::rust_worker::region_token(&bytes) != Some(token) {
            continue;
        }
        let _ = crate::fault_fs::create_dir_all(evidence);
        let _ = crate::fault_fs::write(
            evidence.join(format!(
                "{token}-browser-{}-{}.region",
                std::process::id(),
                REGIONS.fetch_add(1, Ordering::Relaxed)
            )),
            &bytes,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the other side sends, and what is sent to it, failing at the
    /// `left`th read or write and every one after, a byte read at a time.
    struct Failing<'a> {
        input: &'a [u8],
        output: Vec<u8>,
        left: usize,
        failed: bool,
    }

    impl Failing<'_> {
        fn spend(&mut self) -> std::io::Result<()> {
            if self.left == 0 {
                self.failed = true;
                return Err(std::io::Error::other("closed"));
            }
            self.left -= 1;
            Ok(())
        }
    }

    impl Read for Failing<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.spend()?;
            let end = buffer.len().min(1);
            self.input.read(&mut buffer[..end])
        }
    }

    impl Write for Failing<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.spend()?;
            self.output.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Run `read` over `input` failing at each call in turn: every failure
    /// reads nothing, and the run no failure reaches reads `expected`.
    fn fails_each_read<T: PartialEq + std::fmt::Debug>(
        input: &[u8],
        read: impl Fn(&mut Failing<'_>) -> Option<T>,
        expected: T,
    ) {
        for left in 0.. {
            let mut stream = Failing {
                input,
                output: Vec::new(),
                left,
                failed: false,
            };
            let got = read(&mut stream);
            if !stream.failed {
                assert_eq!(got, Some(expected));
                assert!(left > 3, "{left}");
                return;
            }
            assert_eq!(got, None, "failing at {left}");
        }
    }

    #[test]
    fn a_message_whose_stream_fails_anywhere_reads_as_nothing() {
        fails_each_read(
            b"POST /e/2f/session HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3;x=1\r\nabc\r\n0\r\nT: t\r\n\r\n",
            |stream| read_request(&mut BufReader::with_capacity(1, stream)).map(|r| r.body),
            b"abc".to_vec(),
        );
        fails_each_read(
            b"GET /e/2f/status HTTP/1.1\r\nContent-Length: 2\r\n\r\nok",
            |stream| read_request(&mut BufReader::with_capacity(1, stream)).map(|r| r.body),
            b"ok".to_vec(),
        );
        for (response, body) in [
            (
                &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n"[..],
                &b"ok"[..],
            ),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", b"ok"),
            (b"HTTP/1.1 200 OK\r\n\r\nall of it", b"all of it"),
        ] {
            fails_each_read(
                response,
                |stream| read_response(&mut BufReader::with_capacity(1, stream)),
                (200, body.to_vec()),
            );
            let upstream: SocketAddr = "127.0.0.1:9515".parse().unwrap();
            fails_each_read(
                response,
                |stream| exchange(stream, upstream, "POST", "/session", b"{}"),
                (200, body.to_vec()),
            );
        }
        // A line that is no request, a length that is no number and a
        // status that is none read as nothing too.
        for broken in [
            &b"\r\n"[..],
            b"GET\r\n",
            b"GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n",
        ] {
            assert!(read_request(&mut &broken[..]).is_none());
        }
        assert!(read_response(&mut &b"HTTP/1.1 OK\r\n\r\n"[..]).is_none());
        assert!(read_chunked(&mut &b"z\r\n"[..]).is_none());
    }

    #[test]
    fn the_runner_s_browser_is_picked_as_it_picks_it() {
        let env = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            browser(&env(&[("CHROMEDRIVER", "/c/chromedriver")])),
            Some(Browser::Chrome(
                PathBuf::from("/c/chromedriver"),
                Vec::new()
            ))
        );
        assert_eq!(
            browser(&env(&[("CHROMEDRIVER", "/c/cd"), ("GECKODRIVER", "/g/gd")])),
            Some(Browser::Other("geckodriver".into())),
            "the runner tries geckodriver first"
        );
        assert_eq!(
            browser(&env(&[("CHROMEDRIVER_REMOTE", "http://x/")])),
            Some(Browser::Other("a remote chromedriver".into()))
        );
    }

    fn bytes(text: &str) -> std::io::Cursor<Vec<u8>> {
        std::io::Cursor::new(text.as_bytes().to_vec())
    }

    #[test]
    fn a_request_is_read_by_its_length_or_its_chunks() {
        let sized = read_request(&mut bytes(
            "POST /e/00/session HTTP/1.1\r\nHost: x\r\nno colon\r\nContent-Length: 2\r\n\r\n{}tail",
        ))
        .unwrap();
        assert_eq!(
            (sized.method.as_str(), sized.path.as_str()),
            ("POST", "/e/00/session")
        );
        assert_eq!(sized.body, b"{}");
        // Chunks with an extension, then trailers through the blank line.
        let chunked = read_request(&mut bytes(
            "POST /e/00/x HTTP/1.1\r\nTransfer-Encoding: Chunked\r\n\r\n3;n=v\r\n{\"a\r\n3\r\n\":1\r\n1\r\n}\r\n0\r\nX-Trailer: 1\r\n\r\n",
        ))
        .unwrap();
        assert_eq!(chunked.body, br#"{"a":1}"#);
        assert!(read_request(&mut bytes("")).is_none());
        assert!(read_request(&mut bytes("GET\r\n\r\n")).is_none());
        assert!(read_request(&mut bytes("POST / HTTP/1.1\r\nContent-Length: x\r\n\r\n")).is_none());
        // A body cut short, and chunks that stop before their end.
        assert!(
            read_request(&mut bytes("POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\nab")).is_none()
        );
        assert!(
            read_request(&mut bytes(
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n"
            ))
            .is_none()
        );
        assert!(
            read_request(&mut bytes(
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"
            ))
            .is_none()
        );
    }

    #[test]
    fn a_response_is_read_by_its_length_its_chunks_or_its_close() {
        assert_eq!(
            read_response(&mut bytes(
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}more"
            )),
            Some((200, b"{}".to_vec()))
        );
        assert_eq!(
            read_response(&mut bytes(
                "HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\nbad header\r\n\r\n2\r\nno\r\n0\r\n\r\n"
            )),
            Some((404, b"no".to_vec()))
        );
        // Neither: everything until the connection closes.
        assert_eq!(
            read_response(&mut bytes("HTTP/1.1 500 Error\r\n\r\nall of it")),
            Some((500, b"all of it".to_vec()))
        );
        // A chunked body that ends at the end of the stream, with no blank line.
        assert_eq!(
            read_response(&mut bytes(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\n"
            )),
            Some((200, b"x".to_vec()))
        );
        // Another transfer coding is no chunking.
        assert_eq!(
            read_response(&mut bytes(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\nContent-Length: 1\r\n\r\nx"
            )),
            Some((200, b"x".to_vec()))
        );
        assert_eq!(
            read_request(&mut bytes(
                "POST / HTTP/1.1\r\nTransfer-Encoding: identity\r\nContent-Length: 1\r\n\r\nx"
            ))
            .unwrap()
            .body,
            b"x"
        );
        assert!(read_response(&mut bytes("garbage\r\n\r\n")).is_none());
        // A length no sender meant is a body cut short, not an allocation.
        assert!(
            read_response(&mut bytes(
                "HTTP/1.1 200 OK\r\nContent-Length: 999999999999999\r\n\r\n{}"
            ))
            .is_none()
        );
        assert!(
            read_request(&mut bytes(
                "POST / HTTP/1.1\r\nContent-Length: 999999999999999\r\n\r\n{}"
            ))
            .is_none()
        );
    }

    /// The relay between the runner and its WebDriver: a new session gets
    /// the region registry installed, under W3C's answer or the older one;
    /// closing it writes the page's regions that carry the run's token to the
    /// test's evidence; and anything else, a failure included, is passed on
    /// as WebDriver answered it.
    #[test]
    fn the_relay_installs_the_registry_and_takes_the_runs_regions_on_close() {
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};
        let token = "0123456789abcdef";
        let mut ours = vec![0u8; 80];
        ours[56..72].copy_from_slice(token.as_bytes());
        let mut theirs = vec![0u8; 80];
        theirs[56..60].copy_from_slice(b"else");
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let regions = json!({"value": [[0, encode(&ours)], [1, encode(&theirs)], [2, 7]]});
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = upstream.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let mut stream = stream.unwrap();
                let request =
                    read_request(&mut BufReader::new(stream.try_clone().unwrap())).unwrap();
                log.lock()
                    .unwrap()
                    .push(format!("{} {}", request.method, request.path));
                let (status, body) = match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/session") if request.body == b"old" => {
                        (200, json!({"sessionId": "s2", "value": {}}))
                    }
                    // A session refused, an answer that is no JSON, and one
                    // that names no session: nothing is installed.
                    ("POST", "/session") if request.body == b"refused" => {
                        (500, json!({"value": {"error": "session not created"}}))
                    }
                    ("POST", "/session") if request.body == b"text" => (200, json!("{")),
                    ("POST", "/session") if request.body == b"nameless" => {
                        (200, json!({"value": {}}))
                    }
                    ("POST", "/session") => (200, json!({"value": {"sessionId": "s1"}})),
                    ("POST", "/session/s1/execute/sync") => (200, regions.clone()),
                    // Regions refused, sent as no JSON, or none at all.
                    ("POST", "/session/s3/execute/sync") => (500, json!({"value": null})),
                    ("POST", "/session/s4/execute/sync") => (200, json!("{")),
                    ("GET", "/status") => (500, json!({"value": "down"})),
                    _ => (200, json!({"value": null})),
                };
                // `"{"` is sent as the bare `{` it holds: no JSON at all.
                let body = body
                    .as_str()
                    .map_or_else(|| body.to_string(), str::to_owned);
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let evidence = std::env::temp_dir().join(format!(
            "supercov-wasm-relay-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let hex = evidence
            .to_str()
            .unwrap()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let ask = |method: &str, path: &str, body: &str| {
            let mut client = TcpStream::connect(proxy.local_addr().unwrap()).unwrap();
            write!(
                client,
                "{method} /e/{hex}{path} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let (stream, _) = proxy.accept().unwrap();
            relay(stream, address, token).unwrap();
            read_response(&mut BufReader::new(client)).unwrap()
        };
        assert_eq!(ask("POST", "/session", "{}").0, 200);
        assert_eq!(ask("POST", "/session", "old").0, 200);
        assert_eq!(ask("POST", "/session", "refused").0, 500);
        assert_eq!(ask("POST", "/session", "text"), (200, b"{".to_vec()));
        assert_eq!(ask("POST", "/session", "nameless").0, 200);
        let (status, body) = ask("GET", "/status", "");
        assert_eq!((status, body), (500, br#"{"value":"down"}"#.to_vec()));
        assert_eq!(ask("DELETE", "/session/s1/window", "").0, 200);
        assert_eq!(ask("DELETE", "/session/other/deeper", "").0, 200);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                "POST /session",
                "POST /session/s1/goog/cdp/execute",
                "POST /session",
                "POST /session/s2/goog/cdp/execute",
                "POST /session",
                "POST /session",
                "POST /session",
                "GET /status",
                "POST /session/s1/execute/sync",
                "DELETE /session/s1/window",
                "DELETE /session/other/deeper",
            ]
        );
        // A session whose regions cannot be read still closes, writing none.
        for id in ["s3", "s4", "s5"] {
            assert_eq!(ask("DELETE", &format!("/session/{id}"), "").0, 200);
        }
        // A path outside the relay's, or naming an evidence directory that is
        // no text, is not passed on; hex cut short drops its last digit.
        let raw = |path: &str| {
            let mut client = TcpStream::connect(proxy.local_addr().unwrap()).unwrap();
            write!(client, "GET {path} HTTP/1.1\r\nContent-Length: 0\r\n\r\n").unwrap();
            let (stream, _) = proxy.accept().unwrap();
            relay(stream, address, token)
        };
        assert!(raw("/status").is_none());
        assert!(raw("/e/ff/status").is_none());
        assert!(raw("/e/616/status").is_some());
        // A client that hangs up before asking anything, and a driver that
        // no longer listens, are passed nothing.
        {
            let client = TcpStream::connect(proxy.local_addr().unwrap()).unwrap();
            client.shutdown(std::net::Shutdown::Both).unwrap();
            let (stream, _) = proxy.accept().unwrap();
            assert!(relay(stream, address, token).is_none());
        }
        let gone = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let mut client = TcpStream::connect(proxy.local_addr().unwrap()).unwrap();
        write!(
            client,
            "GET /e/{hex}/status HTTP/1.1\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
        let (stream, _) = proxy.accept().unwrap();
        assert!(relay(stream, gone, token).is_none());
        let written = std::fs::read_dir(&evidence)
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(written, [ours]);
        std::fs::remove_dir_all(evidence).unwrap();
    }

    /// A reader with no port for its driver, or none of its own to listen
    /// on, starts nothing.
    #[test]
    fn a_reader_without_its_ports_starts_nothing() {
        for fault in ["local-listener", "local-listener@2"] {
            let started = crate::fault::during(fault, || {
                BrowserReader::start_within(
                    Path::new("/nonexistent/chromedriver"),
                    &[],
                    "0123456789ab",
                    Duration::from_millis(100),
                )
            });
            assert!(started.unwrap_err().contains("injected fault"), "{fault}");
        }
    }

    /// A driver that never listens is stopped, not left running past the
    /// run that started it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_driver_that_never_listens_is_stopped() {
        let directory = std::env::temp_dir().join(format!(
            "supercov-silent-driver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let pid_file = directory.join("pid");
        // `sh -c script --port=N`: the port becomes `$0`, and is ignored.
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let error = BrowserReader::start_within(
            Path::new("/bin/sh"),
            &["-c".into(), script],
            "0123456789ab",
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(error.ends_with("never listened"), "{error}");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "the driver is still running"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
