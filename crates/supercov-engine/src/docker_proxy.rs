//! Containers a suite starts, through any Docker client.
//!
//! The suite's `DOCKER_HOST` points at a socket of Supercov's that relays the
//! Docker Engine API to the daemon the user's own client would reach. Every
//! request passes through unchanged but one: creating a container. A
//! container whose image holds the copy's instrumented code is given the
//! run's settings and Supercov's directory, mounted at the path it has
//! outside, so the code in it writes its evidence where the run reads it.
//! `docker compose`, `docker run`, testcontainers and the SDKs all create
//! containers this way, so one place serves them all.
//!
//! Which images hold the copy's code:
//! - a compose service whose build context is in the copy, which the relay
//!   builds from the copy before the container is created: an image left
//!   over from an earlier run is never used;
//! - any image created while the run was going: the suite built it, from the
//!   copy it runs in;
//! - an image an earlier run recorded as built from source identical to this
//!   run's: a build that found everything cached returns it, unchanged.
//!
//! An image built from an earlier copy is left alone, and the run says so: its
//! probes were placed in other source. At the end the run stops the
//! containers it measured, and the rest of their compose projects, so each
//! writes its last evidence; it never removes them.

use crate::fault_fs as fs;
use crate::fault_process::Launch;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

/// Set in each measured container: its runtime runs in an image of the copy.
pub const CONTAINER_VARIABLE: &str = "SUPERCOV_CONTAINER";

/// What the relay gives the containers it measures.
#[derive(Debug, Clone)]
pub struct ProxySettings {
    /// Supercov's copy: a compose build context in it is the copy's.
    pub copy: PathBuf,
    /// Mounted into each measured container at the same path.
    pub mount: PathBuf,
    /// The run's variables each measured container is given.
    pub environment: Vec<(String, String)>,
    /// The source the run's servers were instrumented from.
    pub fingerprint: String,
    /// Images earlier runs recorded, by id, with their source fingerprint.
    pub registry: PathBuf,
    /// Where the relay records what it did, one JSON line per container.
    pub state: PathBuf,
}

/// The daemon a client would reach without Supercov, as a socket path. None
/// when it is not a local socket (a TCP or SSH daemon), which the relay does
/// not stand in for.
pub fn upstream_socket(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    upstream_socket_through(environment, "docker")
}

/// [`upstream_socket`], asking the context of the CLI at `docker`.
fn upstream_socket_through(
    environment: &BTreeMap<String, String>,
    docker: &str,
) -> Option<PathBuf> {
    if let Some(host) = environment
        .get("DOCKER_HOST")
        .filter(|host| !host.is_empty())
    {
        return host.strip_prefix("unix://").map(PathBuf::from);
    }
    let mut command = Command::new(docker);
    command
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(context) = environment.get("DOCKER_CONTEXT") {
        command.env("DOCKER_CONTEXT", context);
    }
    if let Ok(output) = command.launch_output()
        && output.status.success()
    {
        let host = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return host.strip_prefix("unix://").map(PathBuf::from);
    }
    let default = PathBuf::from("/var/run/docker.sock");
    default.exists().then_some(default)
}

/// The daemon's socket as a container sees it from the daemon's side: what
/// testcontainers mounts into Ryuk, its reaper. Docker Desktop, Colima and
/// the like run the daemon in a VM, where it is /var/run/docker.sock whatever
/// the host calls it; on Linux the daemon shares the host's paths.
#[cfg(target_os = "linux")]
fn daemon_side_socket(upstream: &Path) -> PathBuf {
    upstream.to_owned()
}

#[cfg(not(target_os = "linux"))]
fn daemon_side_socket(_upstream: &Path) -> PathBuf {
    PathBuf::from("/var/run/docker.sock")
}

/// Where the relay listens: in the run's own directory when the path fits a
/// socket address (104 bytes on macOS, 108 on Linux), or else in a private
/// directory of its own under /tmp.
pub fn socket_path(state: &Path) -> io::Result<PathBuf> {
    let preferred = state.join("docker.sock");
    if preferred.as_os_str().len() < 100 {
        fs::create_dir_all(state)?;
        return Ok(preferred);
    }
    let directory = PathBuf::from("/tmp").join(format!(
        "supercov-docker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_private_dir(&directory)?;
    Ok(directory.join("docker.sock"))
}

/// Remove the socket, and the private directory it was given when it had
/// one.
fn remove_socket(socket: &Path) {
    let _ = fs::remove_file(socket);
    if let Some(parent) = socket.parent()
        && parent.starts_with("/tmp")
        && parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("supercov-docker-"))
    {
        let _ = fs::remove_dir(parent);
    }
}

/// A running relay.
pub struct DockerProxy {
    socket: PathBuf,
    upstream: PathBuf,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What every connection's handler reads and records.
struct Shared {
    settings: ProxySettings,
    upstream: PathBuf,
    socket: PathBuf,
    started_ms: i64,
    /// Containers created through the relay, as recorded.
    created: Mutex<Vec<CreatedContainer>>,
    /// Compose services already built from the copy in this run.
    built: Mutex<BTreeSet<(String, String)>>,
    /// The Docker CLI compose is asked through.
    docker: PathBuf,
}

#[derive(Debug, Clone)]
struct CreatedContainer {
    id: String,
    measured: bool,
    project: Option<String>,
}

impl DockerProxy {
    /// Listen on `socket` and relay to `upstream`.
    pub fn start(socket: &Path, upstream: &Path, settings: ProxySettings) -> io::Result<Self> {
        if let Some(parent) = socket.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        listener
            .set_nonblocking(true)
            .expect("a listening socket just bound can be made nonblocking");
        if let Some(parent) = settings.state.parent() {
            fs::create_dir_all(parent)?;
        }
        let shared = Arc::new(Shared {
            settings,
            upstream: upstream.to_owned(),
            socket: socket.to_owned(),
            started_ms: now_ms(),
            created: Mutex::new(Vec::new()),
            built: Mutex::new(BTreeSet::new()),
            docker: PathBuf::from("docker"),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("supercov-docker-relay".into())
                .spawn(move || accept(&listener, &shared, &stop))
                .expect("a thread for the relay starts")
        };
        Ok(Self {
            socket: socket.to_owned(),
            upstream: upstream.to_owned(),
            shared,
            stop,
            thread: Some(thread),
        })
    }

    /// What the suite's environment needs: its clients reach the relay, and
    /// a reaper a library starts mounts the daemon's own socket, which the
    /// relay's could not stand in for inside a VM.
    pub fn environment(&self, ambient: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([(
            "DOCKER_HOST".to_owned(),
            format!("unix://{}", self.socket.display()),
        )]);
        if !ambient.contains_key("TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE") {
            environment.insert(
                "TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE".into(),
                daemon_side_socket(&self.upstream).display().to_string(),
            );
        }
        environment
    }

    /// Stop what the run measured, and the rest of their compose projects, so
    /// each writes its last evidence; then stop relaying. Never removed: the
    /// containers are the suite's.
    pub fn finish(mut self) {
        let created = self
            .shared
            .created
            .lock()
            .map(|created| created.clone())
            .unwrap_or_default();
        let projects = created
            .iter()
            .filter(|container| container.measured)
            .filter_map(|container| container.project.clone())
            .collect::<BTreeSet<_>>();
        for container in &created {
            let ours = container.measured
                || container
                    .project
                    .as_ref()
                    .is_some_and(|project| projects.contains(project));
            if ours {
                let _ = daemon_request(
                    &self.upstream,
                    "POST",
                    &format!("/containers/{}/stop?t=10", container.id),
                    None,
                );
            }
        }
        self.stop.store(true, Ordering::Release);
        // Always there: only `finish`, which takes the relay, takes it.
        let _ = self.thread.take().map(JoinHandle::join);
        remove_socket(&self.socket);
    }
}

impl Drop for DockerProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if self.thread.is_some() {
            remove_socket(&self.socket);
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn accept(listener: &UnixListener, shared: &Arc<Shared>, stop: &AtomicBool) {
    // Looked at once at least, so a relay stopped as soon as it started ends
    // the same way as one stopped later.
    loop {
        match listener.accept() {
            Ok((client, _)) => {
                let _ = client.set_nonblocking(false);
                let shared = Arc::clone(shared);
                let _ = std::thread::Builder::new()
                    .name("supercov-docker-connection".into())
                    .spawn(move || {
                        let _ = relay_connection(client, &shared);
                    });
            }
            // Nothing waiting, or a connection that failed before it was
            // taken: look again shortly.
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
    }
}

/// An HTTP/1.1 message head: the start line and its headers, as sent.
#[derive(Debug, Clone, PartialEq)]
struct Head {
    start: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn without(&mut self, name: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    }

    fn set(&mut self, name: &str, value: String) {
        self.without(name);
        self.headers.push((name.to_owned(), value));
    }

    fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    }

    fn content_length(&self) -> Option<u64> {
        self.header("content-length")?.trim().parse().ok()
    }

    fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut text = String::with_capacity(256);
        text.push_str(&self.start);
        text.push_str("\r\n");
        for (key, value) in &self.headers {
            text.push_str(key);
            text.push_str(": ");
            text.push_str(value);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        out.write_all(text.as_bytes())
    }
}

/// The next message head, or None at a clean end of the stream.
fn read_head(reader: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut start = String::new();
    loop {
        start.clear();
        if reader.read_line(&mut start)? == 0 {
            return Ok(None);
        }
        // Blank lines between messages are tolerated, as RFC 9112 allows.
        if !start.trim().is_empty() {
            break;
        }
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "head cut short",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed header"))?;
        headers.push((key.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(Some(Head {
        start: start.trim_end_matches(['\r', '\n']).to_owned(),
        headers,
    }))
}

/// Copy a body framed by `head` from `reader` to `out`, flushing as it goes:
/// a stream (events, logs, build progress) reaches the client as it comes.
fn relay_body(head: &Head, reader: &mut impl BufRead, out: &mut impl Write) -> io::Result<()> {
    if head.chunked() {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "chunk cut short",
                ));
            }
            out.write_all(size_line.as_bytes())?;
            let size =
                usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("").trim(), 16)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                // Trailers, then the blank line that ends the body.
                loop {
                    let mut trailer = String::new();
                    if reader.read_line(&mut trailer)? == 0 {
                        break;
                    }
                    out.write_all(trailer.as_bytes())?;
                    if trailer.trim().is_empty() {
                        break;
                    }
                }
                out.flush()?;
                return Ok(());
            }
            copy_exact(reader, out, size as u64 + 2)?;
            out.flush()?;
        }
    } else if let Some(length) = head.content_length() {
        copy_exact(reader, out, length)?;
        out.flush()
    } else {
        Ok(())
    }
}

fn copy_exact(reader: &mut impl Read, out: &mut impl Write, mut remaining: u64) -> io::Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let want = buffer.len().min(remaining as usize);
        let read = reader.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "body cut short",
            ));
        }
        out.write_all(&buffer[..read])?;
        remaining -= read as u64;
    }
    Ok(())
}

/// A whole body, decoded.
fn read_body(head: &Head, reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    relay_body(head, reader, &mut encoded)?;
    if !head.chunked() {
        return Ok(encoded);
    }
    // `relay_body` read every size line as text and copied every chunk
    // whole, so what it wrote decodes.
    let mut decoded = Vec::new();
    let mut cursor = io::Cursor::new(encoded);
    loop {
        let mut size_line = String::new();
        cursor
            .read_line(&mut size_line)
            .expect("each size line was read as text");
        let size =
            usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("").trim(), 16)
                .expect("each chunk size was read");
        if size == 0 {
            return Ok(decoded);
        }
        let mut chunk = vec![0; size + 2];
        cursor
            .read_exact(&mut chunk)
            .expect("each chunk was copied whole");
        decoded.extend_from_slice(&chunk[..size]);
    }
}

/// Whether a response ends with the stream handed over to raw bytes both
/// ways: `101` for a request that asked to upgrade (`attach`, `exec`,
/// BuildKit's session), or Docker's raw stream types without a length, the
/// older hijack.
fn hijacked(request: &Head, response: &Head) -> bool {
    let status = response.start.split_whitespace().nth(1).unwrap_or("");
    let asked = request.header("upgrade").is_some()
        || request
            .header("connection")
            .is_some_and(|value| value.to_ascii_lowercase().contains("upgrade"));
    if status == "101" && asked {
        return true;
    }
    let raw = response.header("content-type").is_some_and(|value| {
        value.starts_with("application/vnd.docker.raw-stream")
            || value.starts_with("application/vnd.docker.multiplexed-stream")
    });
    raw && status == "200" && !response.chunked() && response.content_length().is_none()
}

/// Whether a response carries no body whatever its headers say.
fn bodiless(request: &Head, response: &Head) -> bool {
    let status = response.start.split_whitespace().nth(1).unwrap_or("");
    request.start.starts_with("HEAD ")
        || status.starts_with('1')
        || status == "204"
        || status == "304"
}

/// The path of a request, past an API version prefix and before the query.
fn endpoint(head: &Head) -> (&str, &str) {
    let mut parts = head.start.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let path = target.split('?').next().unwrap_or("");
    let path = match path.strip_prefix("/v") {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
            rest.find('/').map_or(path, |index| &rest[index..])
        }
        _ => path,
    };
    (method, path)
}

fn relay_connection(client: UnixStream, shared: &Shared) -> io::Result<()> {
    let upstream = UnixStream::connect(&shared.upstream)?;
    // A connected socket's descriptor duplicates.
    let mut client_reader = BufReader::new(client.try_clone().expect("a socket duplicates"));
    let mut client_writer = client;
    let mut upstream_reader = BufReader::new(upstream.try_clone().expect("a socket duplicates"));
    let mut upstream_writer = upstream;
    if relay_messages(
        &mut client_reader,
        &mut client_writer,
        &mut upstream_reader,
        &mut upstream_writer,
        shared,
    )? {
        return splice(
            client_reader,
            client_writer,
            upstream_reader,
            upstream_writer,
        );
    }
    Ok(())
}

/// Each request from the client and the daemon's answer to it, until either
/// side is done; true when both agreed to hand the connection over to a
/// stream, which the caller splices.
fn relay_messages(
    client_reader: &mut impl BufRead,
    client_writer: &mut impl Write,
    upstream_reader: &mut impl BufRead,
    upstream_writer: &mut impl Write,
    shared: &Shared,
) -> io::Result<bool> {
    loop {
        let Some(mut request) = read_head(client_reader)? else {
            return Ok(false);
        };
        let (method, path) = endpoint(&request);
        let creating = method == "POST" && path == "/containers/create";
        let mut created_context = None;
        if creating {
            let body = read_body(&request, client_reader)?;
            let (body, context) = match serde_json::from_slice::<Value>(&body) {
                Ok(config) => {
                    let (config, context) = shared.instrument(config);
                    (serde_json::to_vec(&config).unwrap_or(body), Some(context))
                }
                Err(_) => (body, None),
            };
            created_context = context;
            request.without("transfer-encoding");
            request.set("Content-Length", body.len().to_string());
            request.write(upstream_writer)?;
            upstream_writer.write_all(&body)?;
            upstream_writer.flush()?;
        } else {
            request.write(upstream_writer)?;
            upstream_writer.flush()?;
            if request
                .header("expect")
                .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
            {
                // The daemon answers `100 Continue` (or refuses) before the
                // body is sent.
                let Some(interim) = read_head(upstream_reader)? else {
                    return Ok(false);
                };
                interim.write(client_writer)?;
                client_writer.flush()?;
                if !interim.start.contains(" 100") {
                    if !bodiless(&request, &interim) {
                        relay_body(&interim, upstream_reader, client_writer)?;
                    }
                    continue;
                }
            }
            relay_body(&request, client_reader, upstream_writer)?;
        }
        let Some(response) = read_head(upstream_reader)? else {
            return Ok(false);
        };
        response.write(client_writer)?;
        client_writer.flush()?;
        if hijacked(&request, &response) {
            return Ok(true);
        }
        if bodiless(&request, &response) {
            continue;
        }
        if let Some(context) = created_context {
            let body = read_body(&response, upstream_reader)?;
            // Recorded before the client hears of it: a client that has its
            // answer may end the run, and the run's end reads the record.
            if let Ok(created) = serde_json::from_slice::<Value>(&body)
                && let Some(id) = created.get("Id").and_then(Value::as_str)
            {
                shared.record(id, context);
            }
            // Sent as it came: chunked re-framed as one chunk.
            if response.chunked() {
                write!(client_writer, "{:x}\r\n", body.len())?;
                client_writer.write_all(&body)?;
                client_writer.write_all(b"\r\n0\r\n\r\n")?;
            } else {
                client_writer.write_all(&body)?;
            }
            client_writer.flush()?;
            continue;
        }
        if !response.chunked() && response.content_length().is_none() {
            // Delimited by the connection closing.
            io::copy(upstream_reader, client_writer)?;
            return Ok(false);
        }
        relay_body(&response, upstream_reader, client_writer)?;
    }
}

/// Raw bytes both ways until either side closes: what was already read into
/// a reader's buffer goes first.
fn splice(
    mut client_reader: BufReader<UnixStream>,
    mut client_writer: UnixStream,
    mut upstream_reader: BufReader<UnixStream>,
    mut upstream_writer: UnixStream,
) -> io::Result<()> {
    let buffered = client_reader.buffer().to_vec();
    client_reader.consume(buffered.len());
    upstream_writer.write_all(&buffered)?;
    let to_daemon = std::thread::spawn(move || {
        let _ = io::copy(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream_reader, &mut client_writer);
    let _ = client_writer.shutdown(std::net::Shutdown::Write);
    let _ = to_daemon.join();
    Ok(())
}

/// One request to the daemon, made by the relay itself: the body of the
/// answer when it succeeded.
fn daemon_request(socket: &Path, method: &str, path: &str, body: Option<&[u8]>) -> Option<Vec<u8>> {
    let stream = UnixStream::connect(socket).ok()?;
    // A connected socket takes a timeout: only an unconnected one refuses.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    daemon_exchange(stream, method, path, body)
}

/// [`daemon_request`] over a stream already open to the daemon.
fn daemon_exchange(
    mut stream: impl Read + Write,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let body = body.unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(body).ok()?;
    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader).ok()??;
    let status = head.start.split_whitespace().nth(1).unwrap_or("");
    if !status.starts_with('2') {
        return None;
    }
    if head.chunked() || head.content_length().is_some() {
        read_body(&head, &mut reader).ok()
    } else {
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).ok()?;
        Some(rest)
    }
}

/// What deciding about one container found, kept until it is created.
#[derive(Debug, Clone, PartialEq)]
struct CreateContext {
    measured: bool,
    image: String,
    project: Option<String>,
    service: Option<String>,
    /// Why it was left alone, when it was.
    note: Option<String>,
}

impl Shared {
    /// A create request's configuration, with the run's settings when its
    /// image holds the copy's code.
    fn instrument(&self, mut config: Value) -> (Value, CreateContext) {
        let image = config
            .get("Image")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let labels = config.get("Labels").cloned().unwrap_or(Value::Null);
        let label = |name: &str| labels.get(name).and_then(Value::as_str).map(str::to_owned);
        let project = label("com.docker.compose.project");
        let service = label("com.docker.compose.service");
        self.rewrite_socket_mounts(&mut config);
        let mut context = CreateContext {
            measured: false,
            image: image.clone(),
            project: project.clone(),
            service: service.clone(),
            note: None,
        };
        // A compose service built from the copy is built from it now, so the
        // container holds the copy as it is.
        let compose_built = match (&project, &service) {
            (Some(project), Some(service)) => self.build_compose_service(
                project,
                service,
                label("com.docker.compose.project.config_files"),
                label("com.docker.compose.project.working_dir"),
            ),
            _ => None,
        };
        let Some(inspected) = daemon_request(
            &self.upstream,
            "GET",
            &format!("/images/{}/json", url_path_segment(&image)),
            None,
        )
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok()) else {
            return (config, context);
        };
        let id = inspected
            .get("Id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let created_ms = inspected
            .get("Created")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms)
            .unwrap_or(0);
        let mut registry = read_registry(&self.settings.registry);
        let decision = if compose_built == Some(true) || created_ms >= self.started_ms {
            registry.insert(id.clone(), self.settings.fingerprint.clone());
            write_registry(&self.settings.registry, &registry);
            Some(true)
        } else if let Some(fingerprint) = registry.get(&id) {
            if *fingerprint == self.settings.fingerprint {
                Some(true)
            } else {
                context.note = Some(format!(
                    "image {image} was built from an earlier copy of the project; build it in the test command to measure it"
                ));
                Some(false)
            }
        } else {
            None
        };
        if decision != Some(true) {
            return (config, context);
        }
        let image_environment = inspected
            .get("Config")
            .and_then(|config| config.get("Env"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|entry| entry.split_once('='))
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        add_settings(&mut config, &self.settings, &image_environment);
        context.measured = true;
        (config, context)
    }

    /// A bind of the relay's own socket is a bind of the daemon's: inside a
    /// VM the relay's path is no socket at all.
    fn rewrite_socket_mounts(&self, config: &mut Value) {
        let ours = self.socket.display().to_string();
        let theirs = daemon_side_socket(&self.upstream).display().to_string();
        let Some(host) = config.get_mut("HostConfig").and_then(Value::as_object_mut) else {
            return;
        };
        if let Some(binds) = host.get_mut("Binds").and_then(Value::as_array_mut) {
            for bind in binds.iter_mut() {
                if let Some(text) = bind.as_str()
                    && let Some(rest) = text.strip_prefix(&ours)
                    && rest.starts_with(':')
                {
                    *bind = json!(format!("{theirs}{rest}"));
                }
            }
        }
        if let Some(mounts) = host.get_mut("Mounts").and_then(Value::as_array_mut) {
            for mount in mounts.iter_mut() {
                if mount.get("Source").and_then(Value::as_str) == Some(ours.as_str()) {
                    mount["Source"] = json!(theirs);
                }
            }
        }
    }

    /// Build a compose service from the copy when its build context is in
    /// it. Some(true) when it was built (now or earlier in this run), Some
    /// (false) when it builds from somewhere else, None when compose could
    /// not say.
    fn build_compose_service(
        &self,
        project: &str,
        service: &str,
        config_files: Option<String>,
        working_dir: Option<String>,
    ) -> Option<bool> {
        if self
            .built
            .lock()
            .is_ok_and(|built| built.contains(&(project.to_owned(), service.to_owned())))
        {
            return Some(true);
        }
        let mut arguments = vec!["compose".to_owned(), "-p".into(), project.to_owned()];
        config_files
            .iter()
            .flat_map(|files| files.split(','))
            .filter(|file| !file.is_empty())
            .for_each(|file| arguments.extend(["-f".into(), file.to_owned()]));
        if let Some(directory) = &working_dir {
            arguments.extend(["--project-directory".into(), directory.clone()]);
        }
        let mut config = Command::new(&self.docker);
        config
            .args(&arguments)
            .args(["config", "--format", "json"])
            .env("DOCKER_HOST", format!("unix://{}", self.upstream.display()))
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let output = config.launch_output().ok()?;
        if !output.status.success() {
            return None;
        }
        let resolved = serde_json::from_slice::<Value>(&output.stdout).ok()?;
        let context = resolved["services"][service]["build"]["context"].as_str()?;
        if crate::fault::or(
            "compose-context-outside",
            || true,
            || !Path::new(context).starts_with(&self.settings.copy),
        ) {
            return Some(false);
        }
        let built = crate::fault::or(
            "compose-build",
            || Some(false),
            || {
                Command::new(&self.docker)
                    .args(&arguments)
                    .args(["build", service])
                    .env("DOCKER_HOST", format!("unix://{}", self.upstream.display()))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .launch_status()
                    .ok()
                    .map(|status| status.success())
            },
        )?;
        if built {
            // Never poisoned: nothing that holds it can panic.
            self.built
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((project.to_owned(), service.to_owned()));
        }
        Some(built)
    }

    fn record(&self, id: &str, context: CreateContext) {
        // Keep the journal append under the same lock as the insertion.
        // JSON Display writes several fragments, which concurrent container
        // creations must not interleave and silently lose their diagnostics.
        let mut created = self.created.lock().unwrap_or_else(PoisonError::into_inner);
        created.push(CreatedContainer {
            id: id.to_owned(),
            measured: context.measured,
            project: context.project.clone(),
        });
        let line = json!({
            "id": id,
            "image": context.image,
            "measured": context.measured,
            "project": context.project,
            "service": context.service,
            "note": context.note,
        });
        if let Ok(mut file) = fs::open_with(
            fs::OpenOptions::new().create(true).append(true),
            &self.settings.state,
        ) {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// A measured container's settings: the run's variables, the mount, and a
/// Python or Ruby runtime in front of what the image itself sets.
fn add_settings(
    config: &mut Value,
    settings: &ProxySettings,
    image_environment: &BTreeMap<String, String>,
) {
    let mut environment = config
        .get("Env")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let own = |environment: &Vec<String>, name: &str| {
        environment
            .iter()
            .any(|entry| entry.split_once('=').is_some_and(|(key, _)| key == name))
    };
    for (name, value) in &settings.environment {
        let value = match name.as_str() {
            "PYTHONPATH" | "RUBYOPT" => {
                joined_with_image(name, value, image_environment.get(name).map(String::as_str))
            }
            _ => value.clone(),
        };
        environment.retain(|entry| entry.split_once('=').is_none_or(|(key, _)| key != name));
        environment.push(format!("{name}={value}"));
    }
    environment.retain(|entry| {
        entry
            .split_once('=')
            .is_none_or(|(key, _)| key != CONTAINER_VARIABLE)
    });
    environment.push(format!("{CONTAINER_VARIABLE}=1"));
    // Python writes the bytecode of Supercov's runtime beside it, in the
    // mounted directory, as the container's root: the run could not remove
    // it. A container's own setting stays.
    if !own(&environment, "PYTHONPYCACHEPREFIX")
        && !image_environment.contains_key("PYTHONPYCACHEPREFIX")
    {
        environment.push("PYTHONPYCACHEPREFIX=/tmp/supercov-pycache".into());
    }
    config["Env"] = json!(environment);
    let mount = settings.mount.display().to_string();
    if !config.get("HostConfig").is_some_and(Value::is_object) {
        config["HostConfig"] = json!({});
    }
    let host = &mut config["HostConfig"];
    let bind = json!(format!("{mount}:{mount}"));
    match host.get_mut("Binds").and_then(Value::as_array_mut) {
        Some(binds) => binds.push(bind),
        None => host["Binds"] = json!([bind]),
    }
}

/// What a variable a runtime needs is in a container whose image sets it
/// too: the runtime's part of ours first, then the image's. Ours was made
/// for the run's own processes and carries the host's value after it.
fn joined_with_image(name: &str, ours: &str, image: Option<&str>) -> String {
    let (runtime, separator) = match name {
        // `<runtime>:<host's PYTHONPATH>`.
        "PYTHONPATH" => (ours.split(':').next().unwrap_or(ours).to_owned(), ":"),
        // `-r<runtime> <host's RUBYOPT>`.
        _ => (
            ours.split_whitespace()
                .filter(|option| option.contains("supercov"))
                .collect::<Vec<_>>()
                .join(" "),
            " ",
        ),
    };
    match image.filter(|value| !value.is_empty()) {
        Some(image) => format!("{runtime}{separator}{image}"),
        None => runtime,
    }
}

/// An image name as one segment of a request path: `/` stays (the daemon
/// takes `repo/name:tag` as written), the rest of the reserved set does not.
fn url_path_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:@".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Milliseconds since the epoch of an RFC 3339 time the daemon prints:
/// `2026-10-17T21:05:37.497328719Z` or with an offset.
fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    // A split's first piece is always there; an empty one parses as nothing.
    let year: i64 = date_parts.next().unwrap_or_default().parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let (time, offset_seconds) = if let Some(time) = rest.strip_suffix('Z') {
        (time, 0)
    } else {
        let index = rest.rfind(['+', '-'])?;
        let (time, offset) = rest.split_at(index);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let mut parts = offset[1..].split(':');
        let hours: i64 = parts.next().unwrap_or_default().parse().ok()?;
        let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
        (time, sign * (hours * 3600 + minutes * 60))
    };
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next().unwrap_or_default().parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    // By characters: a fraction that is not digits is no time, not a panic.
    let millis: i64 = format!("{fraction:0<3}").get(..3)?.parse().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(((days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds) * 1000) + millis)
}

fn read_registry(path: &Path) -> BTreeMap<String, String> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_registry(path: &Path, registry: &BTreeMap<String, String>) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let bytes =
        serde_json::to_vec_pretty(registry).expect("serializing a map of strings cannot fail");
    let temporary = path.with_extension("json.tmp");
    if fs::write(&temporary, bytes).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

/// The notes the relay recorded about containers it left alone.
pub fn notes(state: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(state) else {
        return Vec::new();
    };
    let mut notes = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| line.get("note").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    notes.sort();
    notes.dedup();
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn temporary(name: &str) -> PathBuf {
        // Short: a socket address has room for about a hundred bytes. The
        // counter keeps two tests that read the same clock tick apart.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = PathBuf::from("/tmp").join(format!(
            "scv-dp-{}-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    /// A daemon that answers what these tests ask, and reports each
    /// container configuration it was asked to create.
    fn fake_daemon(socket: &Path) -> mpsc::Receiver<Value> {
        let listener = UnixListener::bind(socket).unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let sender = sender.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    while let Ok(Some(head)) = read_head(&mut reader) {
                        let (method, path) = endpoint(&head);
                        let path = path.to_owned();
                        if head.header("expect").is_some() {
                            // Refused before the body is sent, or let go on.
                            if path == "/refuse" {
                                writer
                                    .write_all(b"HTTP/1.1 417 Expectation Failed\r\nContent-Length: 4\r\n\r\nnope")
                                    .unwrap();
                                continue;
                            }
                            if path == "/decline" {
                                writer
                                    .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                                    .unwrap();
                                continue;
                            }
                            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                        }
                        let body = read_body(&head, &mut reader).unwrap_or_default();
                        if method == "HEAD" {
                            writer
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n")
                                .unwrap();
                            continue;
                        }
                        if path == "/logs-close" {
                            // No length, no chunks: the close ends it.
                            writer
                                .write_all(b"HTTP/1.1 200 OK\r\n\r\nall of it")
                                .unwrap();
                            return;
                        }
                        let respond = |writer: &mut UnixStream, status: &str, body: &str| {
                            write!(
                                writer,
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                                body.len()
                            )
                            .unwrap();
                        };
                        if path.starts_with("/images/") {
                            // Built during the run, pulled long ago, or
                            // built by an earlier run.
                            let (id, created) = if path.contains("pulled") {
                                ("sha256:pulled", "2020-01-01T00:00:00Z")
                            } else if path.contains("stale") {
                                ("sha256:stale", "2020-01-01T00:00:00Z")
                            } else {
                                ("sha256:fresh", "2099-01-01T00:00:00.000Z")
                            };
                            respond(
                                &mut writer,
                                "200 OK",
                                &json!({"Id": id, "Created": created, "Config": {"Env": ["PYTHONPATH=/srv/app"]}}).to_string(),
                            );
                        } else if method == "POST" && path == "/containers/create" {
                            let config: Value = serde_json::from_slice(&body).unwrap();
                            let image = config["Image"].as_str().unwrap_or("").to_owned();
                            sender.send(config).unwrap();
                            // Refused, failed without JSON, chunked, or
                            // created plainly.
                            if image.starts_with("refused") {
                                respond(
                                    &mut writer,
                                    "409 Conflict",
                                    r#"{"message":"name in use"}"#,
                                );
                            } else if image.starts_with("broken") {
                                respond(&mut writer, "500 Internal Server Error", "boom");
                            } else if image.starts_with("chunky") {
                                let body = r#"{"Id":"c0ffee","Warnings":[]}"#;
                                write!(
                                    writer,
                                    "HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                                    body.len()
                                )
                                .unwrap();
                            } else {
                                respond(
                                    &mut writer,
                                    "201 Created",
                                    r#"{"Id":"c0ffee","Warnings":[]}"#,
                                );
                            }
                        } else if path == "/events" {
                            writer
                                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                                .unwrap();
                            for event in ["first", "second"] {
                                write!(writer, "{:x}\r\n{event}\r\n", event.len()).unwrap();
                                writer.flush().unwrap();
                            }
                            writer.write_all(b"0\r\n\r\n").unwrap();
                        } else if path.ends_with("/attach") {
                            writer
                                .write_all(b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
                                .unwrap();
                            // Echo raw bytes until the client closes.
                            let mut buffer = [0_u8; 64];
                            loop {
                                let read = reader.read(&mut buffer).unwrap_or(0);
                                if read == 0 {
                                    break;
                                }
                                writer.write_all(&buffer[..read]).unwrap();
                            }
                            return;
                        } else {
                            respond(&mut writer, "200 OK", "OK");
                        }
                    }
                });
            }
        });
        receiver
    }

    fn settings(directory: &Path) -> ProxySettings {
        ProxySettings {
            copy: directory.join("copy"),
            mount: directory.join("mount"),
            environment: vec![
                ("SUPERCOV_RUN".into(), "run_1".into()),
                ("PYTHONPATH".into(), "/runtime/python:/host/path".into()),
            ],
            fingerprint: "source-a".into(),
            registry: directory.join("registry.json"),
            state: directory.join("state/containers.jsonl"),
        }
    }

    fn exchange(
        stream: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        request: &str,
    ) -> (Head, Vec<u8>) {
        stream.write_all(request.as_bytes()).unwrap();
        let head = read_head(reader).unwrap().unwrap();
        let body = read_body(&head, reader).unwrap();
        (head, body)
    }

    #[test]
    fn requests_pass_through_and_a_created_container_from_the_run_gets_the_settings() {
        let directory = temporary("relay");
        let daemon = directory.join("d.sock");
        let created = fake_daemon(&daemon);
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());

        // Plain requests on one kept-alive connection.
        let (head, body) = exchange(
            &mut stream,
            &mut reader,
            "GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n",
        );
        assert!(head.start.contains("200"), "{head:?}");
        assert_eq!(body, b"OK");
        // A stream arrives as it comes, framed as sent.
        stream
            .write_all(b"GET /v1.47/events HTTP/1.1\r\nHost: docker\r\n\r\n")
            .unwrap();
        let head = read_head(&mut reader).unwrap().unwrap();
        assert!(head.chunked());
        assert_eq!(read_body(&head, &mut reader).unwrap(), b"firstsecond");

        // Created from an image the run built: settings, a joined
        // PYTHONPATH, the mount, and a bind of the relay's socket moved to
        // the daemon's.
        let socket = directory.join("p.sock").display().to_string();
        let config = json!({
            "Image": "app-api",
            "Env": ["MODE=test"],
            "HostConfig": {"Binds": [format!("{socket}:/var/run/docker.sock")]},
        })
        .to_string();
        let (head, body) = exchange(
            &mut stream,
            &mut reader,
            &format!(
                "POST /v1.47/containers/create?name=api HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{config}",
                config.len()
            ),
        );
        assert!(head.start.contains("201"), "{head:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["Id"],
            "c0ffee"
        );
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        let environment = sent["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(
            environment.contains(&"MODE=test".to_owned()),
            "{environment:?}"
        );
        assert!(
            environment.contains(&"SUPERCOV_RUN=run_1".to_owned()),
            "{environment:?}"
        );
        assert!(
            environment.contains(&"PYTHONPATH=/runtime/python:/srv/app".to_owned()),
            "{environment:?}"
        );
        assert!(
            environment.contains(&"SUPERCOV_CONTAINER=1".to_owned()),
            "{environment:?}"
        );
        let binds = sent["HostConfig"]["Binds"].as_array().unwrap();
        let mount = directory.join("mount").display().to_string();
        assert!(
            binds.contains(&json!(format!("{mount}:{mount}"))),
            "{binds:?}"
        );
        assert!(
            binds.contains(&json!(format!(
                "{}:/var/run/docker.sock",
                daemon_side_socket(&daemon).display()
            ))),
            "{binds:?}"
        );
        assert_eq!(
            read_registry(&directory.join("registry.json"))
                .get("sha256:fresh")
                .map(String::as_str),
            Some("source-a")
        );

        // A pulled image is the image's own: nothing added.
        let config = json!({"Image": "pulled/postgres:16"}).to_string();
        exchange(
            &mut stream,
            &mut reader,
            &format!(
                "POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{config}",
                config.len()
            ),
        );
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(sent.get("Env").is_none(), "{sent}");

        // An upgraded stream is spliced both ways.
        let mut attach = UnixStream::connect(directory.join("p.sock")).unwrap();
        attach
            .write_all(b"POST /containers/c0ffee/attach?stream=1 HTTP/1.1\r\nHost: docker\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut attach_reader = BufReader::new(attach.try_clone().unwrap());
        let head = read_head(&mut attach_reader).unwrap().unwrap();
        assert!(head.start.contains("101"), "{head:?}");
        attach.write_all(b"ping").unwrap();
        let mut echoed = [0_u8; 4];
        attach_reader.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        drop(attach);
        drop(stream);
        proxy.finish();
        let state = fs::read_to_string(directory.join("state/containers.jsonl")).unwrap();
        assert_eq!(state.lines().count(), 2, "{state}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_container_creation_keeps_every_note_as_a_complete_record() {
        let directory = temporary("concurrent-records");
        fs::create_dir_all(directory.join("state")).unwrap();
        let shared = Arc::new(Shared {
            settings: settings(&directory),
            upstream: directory.join("d.sock"),
            socket: directory.join("p.sock"),
            started_ms: now_ms(),
            created: Mutex::default(),
            built: Mutex::default(),
            docker: PathBuf::from("docker"),
        });
        let start = Arc::new(std::sync::Barrier::new(8));
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let shared = Arc::clone(&shared);
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    for index in 0..32 {
                        let id = format!("container-{worker}-{index}");
                        shared.record(
                            &id,
                            CreateContext {
                                measured: false,
                                image: format!("image-{worker}-{index}"),
                                project: Some("parallel-compose".into()),
                                service: Some(format!("service-{worker}")),
                                note: Some(format!(
                                    "{id}: image was built from an earlier copy {}",
                                    "x".repeat(2048)
                                )),
                            },
                        );
                    }
                });
            }
        });
        let text = fs::read_to_string(&shared.settings.state).unwrap();
        let records = text
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()
            .expect("every container must publish a complete JSON record");
        assert_eq!(records.len(), 256);
        let ids = records
            .iter()
            .map(|record| record["id"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 256);
        assert_eq!(notes(&shared.settings.state).len(), 256);
        assert_eq!(shared.created.lock().unwrap().len(), 256);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_image_built_from_an_earlier_copy_is_left_alone_and_said_to_be() {
        let directory = temporary("stale");
        let daemon = directory.join("d.sock");
        let created = fake_daemon(&daemon);
        write_registry(
            &directory.join("registry.json"),
            &BTreeMap::from([("sha256:stale".to_owned(), "source-old".to_owned())]),
        );
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let config = json!({"Image": "stale-app"}).to_string();
        exchange(
            &mut stream,
            &mut reader,
            &format!(
                "POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{config}",
                config.len()
            ),
        );
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(sent.get("Env").is_none(), "{sent}");
        drop(stream);
        proxy.finish();
        let notes = notes(&directory.join("state/containers.jsonl"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("earlier copy"), "{notes:?}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn times_the_daemon_prints_are_read_to_the_millisecond() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(
            parse_rfc3339_ms("2026-10-17T21:05:37.497328719Z"),
            Some(1_792_271_137_497)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-17T23:05:37.497+02:00"),
            Some(1_792_271_137_497)
        );
        assert_eq!(parse_rfc3339_ms("not a time"), None);
    }

    #[test]
    fn an_api_version_prefix_and_query_are_not_part_of_the_endpoint() {
        let head = |start: &str| Head {
            start: start.into(),
            headers: Vec::new(),
        };
        assert_eq!(
            endpoint(&head("POST /v1.47/containers/create?name=x HTTP/1.1")),
            ("POST", "/containers/create")
        );
        assert_eq!(
            endpoint(&head("POST /containers/create HTTP/1.1")),
            ("POST", "/containers/create")
        );
        assert_eq!(
            endpoint(&head("GET /volumes/v1/x HTTP/1.1")),
            ("GET", "/volumes/v1/x")
        );
    }

    #[test]
    fn continue_refusal_head_and_close_delimited_answers_cross_the_relay() {
        let directory = temporary("frames");
        let daemon = directory.join("d.sock");
        let _created = fake_daemon(&daemon);
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        // A body that waits for `100 Continue` is sent only after it.
        stream
            .write_all(b"POST /build HTTP/1.1\r\nHost: docker\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n")
            .unwrap();
        let interim = read_head(&mut reader).unwrap().unwrap();
        assert!(interim.start.contains(" 100"), "{interim:?}");
        stream.write_all(b"body").unwrap();
        let head = read_head(&mut reader).unwrap().unwrap();
        assert!(head.start.contains(" 200"), "{head:?}");
        assert_eq!(read_body(&head, &mut reader).unwrap(), b"OK");
        // A refusal carries its own body, and the connection goes on.
        stream
            .write_all(b"POST /refuse HTTP/1.1\r\nHost: docker\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n")
            .unwrap();
        let refused = read_head(&mut reader).unwrap().unwrap();
        assert!(refused.start.contains(" 417"), "{refused:?}");
        assert_eq!(read_body(&refused, &mut reader).unwrap(), b"nope");
        // So does one without a body.
        stream
            .write_all(b"POST /decline HTTP/1.1\r\nHost: docker\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n")
            .unwrap();
        let declined = read_head(&mut reader).unwrap().unwrap();
        assert!(declined.start.contains(" 204"), "{declined:?}");
        // A HEAD answer names a length it does not send.
        stream
            .write_all(b"HEAD /_ping HTTP/1.1\r\nHost: docker\r\n\r\n")
            .unwrap();
        let head = read_head(&mut reader).unwrap().unwrap();
        assert!(head.start.contains(" 200"), "{head:?}");
        let (after, body) = exchange(
            &mut stream,
            &mut reader,
            "GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n",
        );
        assert!(after.start.contains(" 200"), "{after:?}");
        assert_eq!(body, b"OK");
        // No length and no chunks: everything until the daemon closes.
        stream
            .write_all(b"GET /logs-close HTTP/1.1\r\nHost: docker\r\n\r\n")
            .unwrap();
        let closing = read_head(&mut reader).unwrap().unwrap();
        assert!(closing.start.contains(" 200"), "{closing:?}");
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"all of it");
        drop(stream);
        proxy.finish();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_go_into_any_config_shape_and_a_container_s_own_cache_prefix_stays() {
        let directory = PathBuf::from("/tmp/scv-settings");
        let mount = format!("{0}:{0}", directory.join("mount").display());
        let environment = |config: &Value| -> Vec<String> {
            config["Env"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect()
        };
        // No HostConfig at all, and a cache prefix of the container's own.
        let mut bare = json!({"Image": "app", "Env": ["PYTHONPYCACHEPREFIX=/mine", "KEEP=1", "SUPERCOV_CONTAINER=0"]});
        add_settings(&mut bare, &settings(&directory), &BTreeMap::new());
        let env = environment(&bare);
        assert!(
            env.contains(&"PYTHONPYCACHEPREFIX=/mine".to_owned()),
            "{env:?}"
        );
        assert!(
            !env.iter()
                .any(|entry| entry == "PYTHONPYCACHEPREFIX=/tmp/supercov-pycache")
        );
        assert!(
            env.contains(&"KEEP=1".to_owned()) && env.contains(&"SUPERCOV_CONTAINER=1".to_owned())
        );
        assert!(!env.contains(&"SUPERCOV_CONTAINER=0".to_owned()), "{env:?}");
        assert_eq!(bare["HostConfig"]["Binds"], json!([mount]));
        // A HostConfig that is not an object, and one without Binds.
        let mut odd = json!({"HostConfig": null});
        add_settings(&mut odd, &settings(&directory), &BTreeMap::new());
        assert_eq!(odd["HostConfig"]["Binds"], json!([mount]));
        let mut unbound = json!({"HostConfig": {"Binds": "not a list", "Privileged": true}});
        add_settings(&mut unbound, &settings(&directory), &BTreeMap::new());
        assert_eq!(unbound["HostConfig"]["Binds"], json!([mount]));
        assert_eq!(unbound["HostConfig"]["Privileged"], json!(true));
        // The image's own prefix stays too.
        let mut imaged = json!({});
        add_settings(
            &mut imaged,
            &settings(&directory),
            &BTreeMap::from([("PYTHONPYCACHEPREFIX".to_owned(), "/image".to_owned())]),
        );
        assert!(
            !environment(&imaged)
                .iter()
                .any(|entry| entry.starts_with("PYTHONPYCACHEPREFIX="))
        );
    }

    #[test]
    fn a_bind_or_mount_of_the_relay_s_socket_becomes_one_of_the_daemon_s() {
        let directory = temporary("binds");
        let daemon = directory.join("d.sock");
        let created = fake_daemon(&daemon);
        let ours = directory.join("p.sock");
        let proxy = DockerProxy::start(&ours, &daemon, settings(&directory)).unwrap();
        let theirs = daemon_side_socket(&daemon).display().to_string();
        let ours = ours.display().to_string();
        let mut stream = UnixStream::connect(&ours).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        // An image from before the run: only the socket binds change.
        let config = json!({
            "Image": "pulled-app",
            "HostConfig": {
                "Binds": [7, "/data:/data", format!("{ours}:/var/run/docker.sock"), format!("{ours}x:/x")],
                "Mounts": [{"Source": ours, "Target": "/var/run/docker.sock"}, {"Source": "/data"}],
            },
        })
        .to_string();
        exchange(
            &mut stream,
            &mut reader,
            &format!(
                "POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{config}",
                config.len()
            ),
        );
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            sent["HostConfig"]["Binds"],
            json!([
                7,
                "/data:/data",
                format!("{theirs}:/var/run/docker.sock"),
                format!("{ours}x:/x")
            ])
        );
        assert_eq!(sent["HostConfig"]["Mounts"][0]["Source"], json!(theirs));
        assert_eq!(sent["HostConfig"]["Mounts"][1]["Source"], json!("/data"));
        assert!(sent.get("Env").is_none(), "{sent}");
        drop(stream);
        proxy.finish();
        fs::remove_dir_all(directory).unwrap();
    }

    fn head(start: &str, headers: &[(&str, &str)]) -> Head {
        Head {
            start: start.into(),
            headers: headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
        }
    }

    #[test]
    fn a_head_skips_blank_lines_before_it_and_refuses_a_cut_or_malformed_one() {
        let read = |text: &str| read_head(&mut io::Cursor::new(text.as_bytes().to_vec()));
        let parsed = read("\r\n\r\nGET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.start, "GET /_ping HTTP/1.1");
        assert_eq!(parsed.header("host"), Some("docker"));
        assert!(read("").unwrap().is_none());
        assert!(read("\r\n").unwrap().is_none());
        assert_eq!(
            read("GET / HTTP/1.1\r\nHost: docker\r\n")
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            read("GET / HTTP/1.1\r\nno colon here\r\n\r\n")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_body_is_relayed_by_its_framing_and_a_cut_one_is_an_error() {
        let relay = |head: &Head, body: &str| {
            let mut out = Vec::new();
            relay_body(
                head,
                &mut io::Cursor::new(body.as_bytes().to_vec()),
                &mut out,
            )
            .map(|()| String::from_utf8(out).unwrap())
        };
        let chunked = head("HTTP/1.1 200 OK", &[("Transfer-Encoding", "chunked")]);
        // Trailers are passed on, through the blank line that ends them.
        assert_eq!(
            relay(&chunked, "3;ext=1\r\nabc\r\n0\r\nX-Trailer: 1\r\n\r\nNEXT").unwrap(),
            "3;ext=1\r\nabc\r\n0\r\nX-Trailer: 1\r\n\r\n"
        );
        // A stream that ends inside the trailers ends the body there.
        assert_eq!(
            relay(&chunked, "0\r\nX-Trailer: 1\r\n").unwrap(),
            "0\r\nX-Trailer: 1\r\n"
        );
        assert_eq!(
            relay(&chunked, "").unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            relay(&chunked, "zz\r\n").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let sized = head("HTTP/1.1 200 OK", &[("Content-Length", "5")]);
        assert_eq!(relay(&sized, "hello, more").unwrap(), "hello");
        assert_eq!(
            relay(&sized, "hel").unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        // Neither framing: the caller decides (a request has no body).
        assert_eq!(relay(&head("GET / HTTP/1.1", &[]), "ignored").unwrap(), "");
    }

    #[test]
    fn a_chunked_body_is_decoded_whole() {
        let chunked = head("HTTP/1.1 201 Created", &[("Transfer-Encoding", "Chunked")]);
        let body = read_body(
            &chunked,
            &mut io::Cursor::new(b"4\r\n{\"Id\r\n6;x\r\n\":\"c\"}\r\n0\r\n\r\n".to_vec()),
        )
        .unwrap();
        assert_eq!(body, br#"{"Id":"c"}"#);
    }

    #[test]
    fn a_stream_is_handed_over_only_when_both_sides_agreed_to_it() {
        let get = head("POST /containers/c/attach HTTP/1.1", &[]);
        let upgrade = head("POST /exec/e/start HTTP/1.1", &[("Upgrade", "tcp")]);
        let connection = head("POST /session HTTP/1.1", &[("Connection", "Upgrade")]);
        let switching = head("HTTP/1.1 101 UPGRADED", &[]);
        assert!(hijacked(&upgrade, &switching));
        assert!(hijacked(&connection, &switching));
        // A 101 nobody asked for is not a hand-over.
        assert!(!hijacked(&get, &switching));
        let raw = |status: &str, extra: &[(&str, &str)]| {
            let mut headers = vec![("Content-Type", "application/vnd.docker.raw-stream")];
            headers.extend_from_slice(extra);
            head(&format!("HTTP/1.1 {status}"), &headers)
        };
        assert!(hijacked(&get, &raw("200 OK", &[])));
        assert!(hijacked(
            &get,
            &head(
                "HTTP/1.1 200 OK",
                &[("Content-Type", "application/vnd.docker.multiplexed-stream")]
            )
        ));
        assert!(!hijacked(&get, &raw("404 Not Found", &[])));
        assert!(!hijacked(
            &get,
            &raw("200 OK", &[("Transfer-Encoding", "chunked")])
        ));
        assert!(!hijacked(&get, &raw("200 OK", &[("Content-Length", "2")])));
        assert!(!hijacked(
            &get,
            &head("HTTP/1.1 200 OK", &[("Content-Type", "application/json")])
        ));
    }

    #[test]
    fn a_response_to_head_or_a_1xx_204_or_304_carries_no_body() {
        let get = head("GET /images/json HTTP/1.1", &[]);
        let ok = head("HTTP/1.1 200 OK", &[("Content-Length", "2")]);
        assert!(bodiless(&head("HEAD /_ping HTTP/1.1", &[]), &ok));
        assert!(bodiless(&get, &head("HTTP/1.1 100 Continue", &[])));
        assert!(bodiless(&get, &head("HTTP/1.1 204 No Content", &[])));
        assert!(bodiless(&get, &head("HTTP/1.1 304 Not Modified", &[])));
        assert!(!bodiless(&get, &ok));
    }

    #[test]
    fn an_image_name_keeps_its_slashes_and_tag_and_escapes_the_rest() {
        assert_eq!(
            url_path_segment("ghcr.io/org/app:1.2@sha256"),
            "ghcr.io/org/app:1.2@sha256"
        );
        assert_eq!(url_path_segment("a b?c#d"), "a%20b%3Fc%23d");
        assert_eq!(url_path_segment(""), "");
    }

    #[test]
    fn a_time_west_of_utc_is_moved_east() {
        assert_eq!(
            parse_rfc3339_ms("2026-10-17T16:05:37.497-05:00"),
            Some(1_792_271_137_497)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-17T22:05:37+01"),
            Some(1_792_271_137_000)
        );
    }

    /// The daemon a client would reach, by `DOCKER_HOST`: a local socket is
    /// stood in for, a TCP daemon is not. The relay's own socket sits in the
    /// run's directory when it fits an address, and in a private directory
    /// under /tmp, removed with it, when it does not.
    #[test]
    fn the_upstream_and_relay_sockets_are_chosen_by_their_paths() {
        let host = |value: &str| {
            upstream_socket(
                &[("DOCKER_HOST".to_owned(), value.to_owned())]
                    .into_iter()
                    .collect(),
            )
        };
        assert_eq!(
            host("unix:///run/user/docker.sock"),
            Some(PathBuf::from("/run/user/docker.sock"))
        );
        assert_eq!(host("tcp://127.0.0.1:2375"), None);
        // Without a CLI to ask, the usual socket, where there is one.
        let usual = Path::new("/var/run/docker.sock");
        assert_eq!(
            upstream_socket_through(&BTreeMap::new(), "/nonexistent/docker"),
            usual.exists().then(|| usual.to_owned())
        );
        // A base short enough for a socket address; the system temporary
        // directory on macOS is nearly too long on its own.
        let base = PathBuf::from("/tmp").join(format!(
            "sc-relay-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let short = base.join("s");
        assert_eq!(socket_path(&short).unwrap(), short.join("docker.sock"));
        let long = base.join("x".repeat(120));
        let private = socket_path(&long).unwrap();
        assert!(private.starts_with("/tmp"), "{}", private.display());
        let directory = private.parent().unwrap().to_owned();
        assert!(directory.is_dir());
        fs::write(&private, b"").unwrap();
        remove_socket(&private);
        assert!(!directory.exists());
        fs::remove_dir_all(base).unwrap();
    }

    fn create(
        stream: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        config: &Value,
    ) -> (Head, Vec<u8>) {
        let config = config.to_string();
        exchange(
            stream,
            reader,
            &format!(
                "POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{config}",
                config.len()
            ),
        )
    }

    #[test]
    fn a_create_the_daemon_refuses_is_passed_on_and_not_recorded() {
        let directory = temporary("refused");
        let daemon = directory.join("d.sock");
        let _created = fake_daemon(&daemon);
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let (head, body) = create(&mut stream, &mut reader, &json!({"Image": "refused-app"}));
        assert!(head.start.contains(" 409"), "{head:?}");
        assert_eq!(body, br#"{"message":"name in use"}"#);
        let (head, body) = create(&mut stream, &mut reader, &json!({"Image": "broken-app"}));
        assert!(head.start.contains(" 500"), "{head:?}");
        assert_eq!(body, b"boom");
        // A chunked answer reaches the client chunked, and is recorded.
        let (head, body) = create(&mut stream, &mut reader, &json!({"Image": "chunky-app"}));
        assert!(head.chunked(), "{head:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["Id"],
            "c0ffee"
        );
        drop(stream);
        proxy.finish();
        let state = fs::read_to_string(directory.join("state/containers.jsonl")).unwrap();
        assert_eq!(state.lines().count(), 1, "{state}");
        assert!(state.contains("chunky-app"), "{state}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_image_an_earlier_run_built_from_the_same_source_is_measured() {
        let directory = temporary("same");
        let daemon = directory.join("d.sock");
        let created = fake_daemon(&daemon);
        write_registry(
            &directory.join("registry.json"),
            &BTreeMap::from([("sha256:stale".to_owned(), "source-a".to_owned())]),
        );
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        create(&mut stream, &mut reader, &json!({"Image": "stale-app"}));
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            sent["Env"]
                .as_array()
                .is_some_and(|env| env.contains(&json!("SUPERCOV_RUN=run_1"))),
            "{sent}"
        );
        drop(stream);
        proxy.finish();
        assert!(notes(&directory.join("state/containers.jsonl")).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_compose_service_built_earlier_in_the_run_is_measured_without_building_again() {
        let directory = temporary("built");
        let daemon = directory.join("d.sock");
        let created = fake_daemon(&daemon);
        let proxy =
            DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory)).unwrap();
        proxy
            .shared
            .built
            .lock()
            .unwrap()
            .insert(("shop".to_owned(), "api".to_owned()));
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        // Pulled long ago, but the service was built from the copy.
        create(
            &mut stream,
            &mut reader,
            &json!({
                "Image": "pulled-api",
                "Labels": {"com.docker.compose.project": "shop", "com.docker.compose.service": "api"},
            }),
        );
        let sent = created.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(sent.get("Env").is_some(), "{sent}");
        assert_eq!(
            read_registry(&directory.join("registry.json"))
                .get("sha256:pulled")
                .map(String::as_str),
            Some("source-a")
        );
        drop(stream);
        proxy.finish();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_compose_project_compose_cannot_read_is_not_built() {
        let directory = temporary("unread");
        let shared = Shared {
            settings: settings(&directory),
            upstream: directory.join("d.sock"),
            socket: directory.join("p.sock"),
            started_ms: now_ms(),
            created: Mutex::default(),
            built: Mutex::default(),
            docker: PathBuf::from("docker"),
        };
        // An empty name in the list is skipped; the file named is missing.
        let files = format!("{},", directory.join("missing.yml").display());
        assert_eq!(
            shared.build_compose_service("shop", "api", Some(files), None),
            None
        );
        assert!(shared.built.lock().unwrap().is_empty());
        // Compose that cannot be started, that says something other than
        // JSON, or that can be asked but not built with: none of it builds.
        let stand_in = |name: &str, script: &str| {
            let path = directory.join(name);
            fs::write(&path, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            Shared {
                docker: path,
                settings: settings(&directory),
                upstream: directory.join("d.sock"),
                socket: directory.join("p.sock"),
                started_ms: now_ms(),
                created: Mutex::default(),
                built: Mutex::default(),
            }
        };
        let absent = Shared {
            docker: directory.join("no-docker"),
            ..stand_in("unused", "#!/bin/sh\n")
        };
        let context = directory.join("copy/api").display().to_string();
        for shared in [
            absent,
            stand_in("garbled", "#!/bin/sh\necho not json\n"),
            stand_in(
                "unbuildable",
                &format!(
                    "#!/bin/sh\nrm \"$0\"\necho '{{\"services\":{{\"api\":{{\"build\":{{\"context\":\"{context}\"}}}}}}}}'\n"
                ),
            ),
        ] {
            assert_eq!(
                shared.build_compose_service("shop", "api", None, None),
                None
            );
            assert!(shared.built.lock().unwrap().is_empty());
        }
        // Empty binds and mounts have nothing to rewrite, and no settings
        // add only what marks the container as the run's.
        let mut config = json!({"HostConfig": {"Binds": [], "Mounts": []}});
        shared.rewrite_socket_mounts(&mut config);
        assert_eq!(config, json!({"HostConfig": {"Binds": [], "Mounts": []}}));
        let bare = ProxySettings {
            environment: Vec::new(),
            ..settings(&directory)
        };
        let mut config = json!({});
        add_settings(&mut config, &bare, &BTreeMap::new());
        assert_eq!(
            config["Env"],
            json!([
                "SUPERCOV_CONTAINER=1",
                "PYTHONPYCACHEPREFIX=/tmp/supercov-pycache"
            ])
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_reaper_socket_the_suite_sets_itself_is_kept() {
        let directory = temporary("reaper");
        let proxy = DockerProxy::start(
            &directory.join("p.sock"),
            &directory.join("d.sock"),
            settings(&directory),
        )
        .unwrap();
        let ambient = BTreeMap::from([(
            "TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE".to_owned(),
            "/mine.sock".to_owned(),
        )]);
        let environment = proxy.environment(&ambient);
        assert!(environment.contains_key("DOCKER_HOST"));
        assert!(!environment.contains_key("TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE"));
        assert!(
            proxy
                .environment(&BTreeMap::new())
                .contains_key("TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE")
        );
        proxy.finish();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_relay_answers_when_its_record_cannot_be_written_but_needs_a_socket_path() {
        let directory = temporary("unrecorded");
        let daemon = directory.join("d.sock");
        let _created = fake_daemon(&daemon);
        assert!(DockerProxy::start(Path::new("/"), &daemon, settings(&directory)).is_err());
        // The root directory: no parent to make, and no file to append to.
        let unwritable = ProxySettings {
            state: PathBuf::from("/"),
            ..settings(&directory)
        };
        let proxy = DockerProxy::start(&directory.join("p.sock"), &daemon, unwritable).unwrap();
        let mut stream = UnixStream::connect(directory.join("p.sock")).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let (head, _) = create(&mut stream, &mut reader, &json!({"Image": "app"}));
        assert!(head.start.contains(" 201"), "{head:?}");
        drop(stream);
        proxy.finish();
        assert!(notes(Path::new("/")).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    /// The relay's own directories that cannot be made refuse it, and a
    /// daemon gone after it started leaves a client with no answer.
    #[test]
    fn a_relay_whose_directories_cannot_be_made_or_whose_daemon_is_gone_says_so() {
        let directory = temporary("relay-steps");
        let daemon = directory.join("d.sock");
        let _created = fake_daemon(&daemon);
        assert!(
            crate::fault::during("fs.create_dir_all#docker_proxy.rs:125", || {
                socket_path(&directory.join("state"))
            })
            .is_err()
        );
        // A run directory too deep for a socket address gets a private
        // directory under /tmp instead, which can fail to be made too.
        assert!(
            crate::fault::during("fs.create_dir#docker_proxy.rs:136", || {
                socket_path(&directory.join("state").join("d".repeat(120)))
            })
            .is_err()
        );
        assert!(
            crate::fault::during("fs.create_dir_all#docker_proxy.rs:197", || {
                DockerProxy::start(&directory.join("p.sock"), &daemon, settings(&directory))
            })
            .is_err()
        );
        let gone = directory.join("gone.sock");
        let proxy =
            DockerProxy::start(&directory.join("q.sock"), &gone, settings(&directory)).unwrap();
        let mut stream = UnixStream::connect(directory.join("q.sock")).unwrap();
        stream
            .write_all(b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n")
            .unwrap();
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));
        proxy.finish();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_registry_that_cannot_be_written_is_left_alone() {
        let directory = temporary("registry");
        let registry = BTreeMap::from([("sha256:a".to_owned(), "source-a".to_owned())]);
        // No parent at all, and a parent that is a file.
        write_registry(Path::new("/"), &registry);
        fs::write(directory.join("file"), b"").unwrap();
        let under_file = directory.join("file/registry.json");
        write_registry(&under_file, &registry);
        assert!(read_registry(&under_file).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_relay_socket_outside_a_private_directory_leaves_its_directory() {
        // Named like a private directory, but not under /tmp.
        let directory = PathBuf::from(format!("supercov-docker-{}-kept", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let socket = directory.join("docker.sock");
        fs::write(&socket, b"").unwrap();
        remove_socket(&socket);
        let kept = directory.is_dir();
        let socket_gone = !socket.exists();
        fs::remove_dir_all(&directory).unwrap();
        assert!(kept && socket_gone);
        // A path with no parent removes nothing else.
        remove_socket(Path::new("/"));
        assert!(Path::new("/").is_dir());
    }

    #[test]
    fn a_docker_context_with_no_endpoint_falls_back_to_the_default_socket() {
        let upstream = upstream_socket(&BTreeMap::from([(
            "DOCKER_CONTEXT".to_owned(),
            format!("supercov-no-such-context-{}", std::process::id()),
        )]));
        let default = PathBuf::from("/var/run/docker.sock");
        assert_eq!(upstream, default.exists().then_some(default));
    }

    /// A time written any other way is no time: never a panic, whatever is
    /// where a number belongs.
    #[test]
    fn a_time_written_wrong_is_no_time() {
        for text in [
            "2026-10-17",
            "x-10-17T21:05:37Z",
            "2026-10-17T",
            "2026-x-17T21:05:37Z",
            "2026-10T21:05:37Z",
            "2026T21:05:37Z",
            "2026-10-xT21:05:37Z",
            "2026--T21:05:37Z",
            "2026-10-17T21:05:37",
            "2026-10-17T21:05:37+x",
            "2026-10-17T21:05:37+05:x",
            "2026-10-17Tx:05:37Z",
            "2026-10-17T21Z",
            "2026-10-17T21:x:37Z",
            "2026-10-17T21:05Z",
            "2026-10-17T21:05:xZ",
            "2026-10-17T21:05:37.xZ",
            "2026-10-17T21:05:37.\u{e9}\u{e9}Z",
        ] {
            assert_eq!(parse_rfc3339_ms(text), None, "{text}");
        }
        assert!(parse_rfc3339_ms("2026-10-17T21:05:37+05Z").is_none());
        assert!(parse_rfc3339_ms("2026-10-17T21:05:37.5+05").is_some());
    }

    /// A stream that fails at its `n`th read or write, as a socket the other
    /// side closed does.
    struct Faulty<T> {
        inner: T,
        left: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl<T> Faulty<T> {
        fn count_call(&self) -> io::Result<()> {
            match self.left.get() {
                // Spent: from now on every call fails, and says it did by
                // leaving the count at its largest.
                0 | usize::MAX => {
                    self.left.set(usize::MAX);
                    Err(io::Error::other("closed"))
                }
                left => {
                    self.left.set(left - 1);
                    Ok(())
                }
            }
        }
    }

    impl<T: Read> Read for Faulty<T> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.count_call()?;
            // A byte at a time, so each line takes many reads.
            let end = buffer.len().min(1);
            self.inner.read(&mut buffer[..end])
        }
    }

    impl<T: Write> Write for Faulty<T> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.count_call()?;
            self.inner.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.count_call()?;
            self.inner.flush()
        }
    }

    /// One connection's messages, relayed over streams that each fail at
    /// the `left`th call any of them makes; what the daemon was sent and the
    /// client answered, or the relay's error.
    fn relay_with(
        shared: &Shared,
        client: &[u8],
        daemon: &[u8],
        left: usize,
    ) -> (io::Result<bool>, Vec<u8>, Vec<u8>, bool) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(left));
        let faulty = |inner| Faulty {
            inner,
            left: calls.clone(),
        };
        let mut client_reader = BufReader::new(faulty(client));
        let mut upstream_reader = BufReader::new(faulty(daemon));
        let mut client_writer = Faulty {
            inner: Vec::new(),
            left: calls.clone(),
        };
        let mut upstream_writer = Faulty {
            inner: Vec::new(),
            left: calls.clone(),
        };
        let relayed = relay_messages(
            &mut client_reader,
            &mut client_writer,
            &mut upstream_reader,
            &mut upstream_writer,
            shared,
        );
        let failed = calls.get() == usize::MAX;
        (relayed, upstream_writer.inner, client_writer.inner, failed)
    }

    /// Both ends of a connection in memory: what the other side sends, and
    /// what is sent to it.
    struct Duplex<'a> {
        input: &'a [u8],
        output: Vec<u8>,
    }

    impl Read for Duplex<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Duplex<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The relay's own request to the daemon, over a stream that fails at
    /// each call in turn: a failure is no answer, and with none the body of
    /// a sized or a close-delimited answer is the answer, a refusal none.
    #[test]
    fn a_request_of_the_relay_s_own_fails_to_no_answer() {
        for (answer, body) in [
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK"[..],
                Some(&b"OK"[..]),
            ),
            (b"HTTP/1.1 200 OK\r\n\r\nall of it", Some(b"all of it")),
            (b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", None),
        ] {
            let mut nth = 0;
            loop {
                let left = std::rc::Rc::new(std::cell::Cell::new(nth));
                let stream = Faulty {
                    inner: Duplex {
                        input: answer,
                        output: Vec::new(),
                    },
                    left: left.clone(),
                };
                let answered = daemon_exchange(stream, "GET", "/images/x/json", Some(b"{}"));
                if left.get() != usize::MAX {
                    assert_eq!(answered.as_deref(), body);
                    break;
                }
                assert_eq!(answered, None, "{nth}");
                nth += 1;
            }
            assert!(nth > 3, "{nth}");
        }
        // A daemon that closes before it answers answers nothing.
        let silent = Duplex {
            input: b"",
            output: Vec::new(),
        };
        assert_eq!(daemon_exchange(silent, "GET", "/_ping", None), None);
    }

    /// Every kind of exchange on one connection -- a continue granted and
    /// one refused, a create the daemon answers chunked and one it answers
    /// sized, a create whose body is not JSON, and an answer the daemon's
    /// closing ends -- relayed once for each read or write with that one
    /// failing: each failure is the relay's error, never a panic.
    #[test]
    fn a_connection_whose_stream_fails_anywhere_ends_with_an_error() {
        let directory = temporary("relay-faults");
        let shared = Shared {
            settings: settings(&directory),
            upstream: directory.join("d.sock"),
            socket: directory.join("p.sock"),
            started_ms: now_ms(),
            created: Mutex::default(),
            built: Mutex::default(),
            docker: PathBuf::from("docker"),
        };
        let create = br#"{"Image":"shop"}"#;
        let client = [
            &b"POST /build HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\nbody"[..],
            b"POST /refuse HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
            format!(
                "POST /containers/create HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                create.len()
            )
            .as_bytes(),
            create,
            format!(
                "POST /v1.43/containers/create HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                create.len()
            )
            .as_bytes(),
            create,
            b"POST /containers/create HTTP/1.1\r\nContent-Length: 1\r\n\r\nx",
            b"GET /logs HTTP/1.1\r\n\r\n",
        ]
        .concat();
        let daemon = [
            &b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK"[..],
            b"HTTP/1.1 417 Expectation Failed\r\nContent-Length: 4\r\n\r\nnope",
            b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\nC\r\n{\"Id\":\"one\"}\r\n0\r\n\r\n",
            b"HTTP/1.1 201 Created\r\nContent-Length: 12\r\n\r\n{\"Id\":\"two\"}",
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\n\r\nno",
            b"HTTP/1.1 200 OK\r\n\r\nall of it",
        ]
        .concat();
        let mut nth = 0;
        loop {
            let (relayed, _, answered, failed) = relay_with(&shared, &client, &daemon, nth);
            if !failed {
                assert!(!relayed.unwrap());
                let answered = String::from_utf8(answered).unwrap();
                assert!(answered.ends_with("all of it"), "{answered}");
                assert!(
                    answered.contains("c\r\n{\"Id\":\"one\"}\r\n0\r\n\r\n"),
                    "{answered}"
                );
                break;
            }
            assert!(relayed.is_err(), "{nth}");
            nth += 1;
        }
        assert!(nth > 100, "{nth}");
        // A daemon gone before it answers ends the connection quietly, as
        // does a client that asked to hand it over to a stream.
        for client in [
            &b"GET /_ping HTTP/1.1\r\n\r\n"[..],
            b"POST /build HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
        ] {
            assert!(!relay_with(&shared, client, b"", usize::MAX - 1).0.unwrap());
        }
        let (handed, _, _, _) = relay_with(
            &shared,
            b"POST /attach HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
            b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
            usize::MAX - 1,
        );
        assert!(handed.unwrap());
        fs::remove_dir_all(directory).unwrap();
    }

    /// Relaying a message whose stream fails at each read in turn, and then
    /// at each write: every failure is an error, and with none the message
    /// crosses whole.
    /// What the client sent ahead of a handed-over stream that the daemon,
    /// gone already, cannot take ends the stream with that error.
    #[test]
    fn a_handed_over_stream_whose_daemon_has_gone_ends_with_an_error() {
        let (client, mut sent) = UnixStream::pair().unwrap();
        sent.write_all(b"raw bytes").unwrap();
        let mut client_reader = BufReader::new(client.try_clone().unwrap());
        assert!(!client_reader.fill_buf().unwrap().is_empty());
        let (upstream, daemon) = UnixStream::pair().unwrap();
        drop(daemon);
        let upstream_reader = BufReader::new(upstream.try_clone().unwrap());
        assert!(splice(client_reader, client, upstream_reader, upstream).is_err());
    }

    #[test]
    fn a_stream_that_fails_anywhere_fails_the_relay_and_never_panics() {
        let chunked = head("HTTP/1.1 200 OK", &[("Transfer-Encoding", "chunked")]);
        let sized = head("HTTP/1.1 200 OK", &[("Content-Length", "5")]);
        let messages: [(&Head, &[u8]); 2] = [
            (
                &chunked,
                b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\nTrailer: t\r\n\r\n",
            ),
            (&sized, b"hello"),
        ];
        for (head, body) in messages {
            for failing_reads in [true, false] {
                let mut nth = 0;
                loop {
                    let left = std::rc::Rc::new(std::cell::Cell::new(nth));
                    let unlimited = std::rc::Rc::new(std::cell::Cell::new(usize::MAX - 1));
                    let (reads, writes) = if failing_reads {
                        (left.clone(), unlimited)
                    } else {
                        (unlimited, left.clone())
                    };
                    let mut reader = io::BufReader::with_capacity(
                        1,
                        Faulty {
                            inner: body,
                            left: reads,
                        },
                    );
                    let mut out = Faulty {
                        inner: Vec::new(),
                        left: writes,
                    };
                    let relayed = relay_body(head, &mut reader, &mut out);
                    if left.get() != usize::MAX {
                        relayed.unwrap();
                        assert_eq!(out.inner, body);
                        break;
                    }
                    assert!(relayed.is_err(), "{nth} {failing_reads}");
                    nth += 1;
                }
                assert!(nth > 2, "{nth}");
            }
            // Read whole, failing at each read in turn.
            let mut nth = 0;
            loop {
                let left = std::rc::Rc::new(std::cell::Cell::new(nth));
                let mut reader = io::BufReader::with_capacity(
                    1,
                    Faulty {
                        inner: body,
                        left: left.clone(),
                    },
                );
                let read = read_body(head, &mut reader);
                if left.get() != usize::MAX {
                    read.unwrap();
                    break;
                }
                assert!(read.is_err());
                nth += 1;
            }
        }
        // A head cut by a failing read, at each read.
        let text: &[u8] = b"\r\nGET / HTTP/1.1\r\nHost: docker\r\n\r\n";
        let mut nth = 0;
        loop {
            let left = std::rc::Rc::new(std::cell::Cell::new(nth));
            let mut reader = io::BufReader::with_capacity(
                1,
                Faulty {
                    inner: text,
                    left: left.clone(),
                },
            );
            let read = read_head(&mut reader);
            if left.get() != usize::MAX {
                assert!(read.unwrap().is_some());
                break;
            }
            assert!(read.is_err());
            nth += 1;
        }
        assert!(nth > 10, "{nth}");
    }
}
