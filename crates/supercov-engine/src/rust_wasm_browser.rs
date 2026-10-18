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

use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
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
    let variable = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(name).ok())
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
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for driver in DRIVERS {
            let candidate = directory
                .join(driver)
                .with_extension(std::env::consts::EXE_EXTENSION);
            if candidate.exists() {
                return Some(pick(driver, candidate));
            }
        }
    }
    None
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
        let port = free_port()?;
        let driver = Command::new(chromedriver)
            .args(arguments)
            .arg(format!("--port={port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", chromedriver.display()))?;
        let upstream = SocketAddr::from(([127, 0, 0, 1], port));
        let started = Instant::now();
        while TcpStream::connect_timeout(&upstream, Duration::from_millis(100)).is_err() {
            if started.elapsed() > Duration::from_secs(20) {
                return Err(format!("{} never listened", chromedriver.display()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let token = token.to_owned();
        std::thread::Builder::new()
            .name("supercov-browser-proxy".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
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
                }
            })
            .map_err(|error| error.to_string())?;
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

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
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
        let mut body = vec![0; length];
        stream.read_exact(&mut body).ok()?;
        body
    };
    Some(Request { method, path, body })
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
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
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
    let mut stream = TcpStream::connect(upstream).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .ok()?;
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
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).ok()?;
        payload
    } else {
        let mut payload = Vec::new();
        reader.read_to_end(&mut payload).ok()?;
        payload
    };
    Some((status, payload))
}

static REGIONS: AtomicU64 = AtomicU64::new(0);

fn relay(stream: TcpStream, upstream: SocketAddr, token: &str) -> Option<()> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
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
        let _ = std::fs::create_dir_all(evidence);
        let _ = std::fs::write(
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
        assert!(read_response(&mut bytes("garbage\r\n\r\n")).is_none());
    }
}
