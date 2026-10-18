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

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
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
    if let Some(host) = environment
        .get("DOCKER_HOST")
        .filter(|host| !host.is_empty())
    {
        return host.strip_prefix("unix://").map(PathBuf::from);
    }
    let mut command = Command::new("docker");
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
    if let Ok(output) = command.output()
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
fn daemon_side_socket(upstream: &Path) -> PathBuf {
    if cfg!(target_os = "linux") {
        upstream.to_owned()
    } else {
        PathBuf::from("/var/run/docker.sock")
    }
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
    use std::os::unix::fs::DirBuilderExt;
    let directory = PathBuf::from("/tmp").join(format!(
        "supercov-docker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
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
        listener.set_nonblocking(true)?;
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
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("supercov-docker-relay".into())
                .spawn(move || accept(&listener, &shared, &stop))?
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
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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
    while !stop.load(Ordering::Acquire) {
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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
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
    let mut decoded = Vec::new();
    let mut cursor = io::Cursor::new(encoded);
    loop {
        let mut size_line = String::new();
        cursor.read_line(&mut size_line)?;
        let size =
            usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            return Ok(decoded);
        }
        let mut chunk = vec![0; size + 2];
        cursor.read_exact(&mut chunk)?;
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
    let mut client_reader = BufReader::new(client.try_clone()?);
    let mut client_writer = client;
    let mut upstream_reader = BufReader::new(upstream.try_clone()?);
    let mut upstream_writer = upstream;
    loop {
        let Some(mut request) = read_head(&mut client_reader)? else {
            return Ok(());
        };
        let (method, path) = endpoint(&request);
        let creating = method == "POST" && path == "/containers/create";
        let mut created_context = None;
        if creating {
            let body = read_body(&request, &mut client_reader)?;
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
            request.write(&mut upstream_writer)?;
            upstream_writer.write_all(&body)?;
            upstream_writer.flush()?;
        } else {
            request.write(&mut upstream_writer)?;
            upstream_writer.flush()?;
            if request
                .header("expect")
                .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
            {
                // The daemon answers `100 Continue` (or refuses) before the
                // body is sent.
                let Some(interim) = read_head(&mut upstream_reader)? else {
                    return Ok(());
                };
                interim.write(&mut client_writer)?;
                client_writer.flush()?;
                if !interim.start.contains(" 100") {
                    if !bodiless(&request, &interim) {
                        relay_body(&interim, &mut upstream_reader, &mut client_writer)?;
                    }
                    continue;
                }
            }
            relay_body(&request, &mut client_reader, &mut upstream_writer)?;
        }
        let Some(response) = read_head(&mut upstream_reader)? else {
            return Ok(());
        };
        response.write(&mut client_writer)?;
        client_writer.flush()?;
        if hijacked(&request, &response) {
            return splice(
                client_reader,
                client_writer,
                upstream_reader,
                upstream_writer,
            );
        }
        if bodiless(&request, &response) {
            continue;
        }
        if let Some(context) = created_context {
            let body = read_body(&response, &mut upstream_reader)?;
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
            io::copy(&mut upstream_reader, &mut client_writer)?;
            return Ok(());
        }
        relay_body(&response, &mut upstream_reader, &mut client_writer)?;
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
    let mut stream = UnixStream::connect(socket).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .ok()?;
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
        for file in config_files.iter().flat_map(|files| files.split(',')) {
            if !file.is_empty() {
                arguments.extend(["-f".into(), file.to_owned()]);
            }
        }
        if let Some(directory) = &working_dir {
            arguments.extend(["--project-directory".into(), directory.clone()]);
        }
        let mut config = Command::new("docker");
        config
            .args(&arguments)
            .args(["config", "--format", "json"])
            .env("DOCKER_HOST", format!("unix://{}", self.upstream.display()))
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let output = config.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let resolved = serde_json::from_slice::<Value>(&output.stdout).ok()?;
        let context = resolved["services"][service]["build"]["context"].as_str()?;
        if !Path::new(context).starts_with(&self.settings.copy) {
            return Some(false);
        }
        let built = Command::new("docker")
            .args(&arguments)
            .args(["build", service])
            .env("DOCKER_HOST", format!("unix://{}", self.upstream.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success();
        if built && let Ok(mut done) = self.built.lock() {
            done.insert((project.to_owned(), service.to_owned()));
        }
        Some(built)
    }

    fn record(&self, id: &str, context: CreateContext) {
        if let Ok(mut created) = self.created.lock() {
            created.push(CreatedContainer {
                id: id.to_owned(),
                measured: context.measured,
                project: context.project.clone(),
            });
        }
        let line = json!({
            "id": id,
            "image": context.image,
            "measured": context.measured,
            "project": context.project,
            "service": context.service,
            "note": context.note,
        });
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.settings.state)
        {
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
    if !host.get("Binds").is_some_and(Value::is_array) {
        host["Binds"] = json!([]);
    }
    if let Some(binds) = host["Binds"].as_array_mut() {
        binds.push(json!(format!("{mount}:{mount}")));
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
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let (time, offset_seconds) = if let Some(time) = rest.strip_suffix('Z') {
        (time, 0)
    } else {
        let index = rest.rfind(['+', '-'])?;
        let (time, offset) = rest.split_at(index);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let mut parts = offset[1..].split(':');
        let hours: i64 = parts.next()?.parse().ok()?;
        let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
        (time, sign * (hours * 3600 + minutes * 60))
    };
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    let millis: i64 = format!("{fraction:0<3}")[..3].parse().ok()?;
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
    if let Ok(bytes) = serde_json::to_vec_pretty(registry) {
        let temporary = path.with_extension("json.tmp");
        if fs::write(&temporary, bytes).is_ok() {
            let _ = fs::rename(temporary, path);
        }
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
        // Short: a socket address has room for about a hundred bytes.
        let path = PathBuf::from("/tmp").join(format!(
            "scv-dp-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        ));
        fs::create_dir_all(&path).unwrap();
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
                        let body = read_body(&head, &mut reader).unwrap_or_default();
                        let (method, path) = endpoint(&head);
                        let path = path.to_owned();
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
                            sender.send(serde_json::from_slice(&body).unwrap()).unwrap();
                            respond(
                                &mut writer,
                                "201 Created",
                                r#"{"Id":"c0ffee","Warnings":[]}"#,
                            );
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
}
