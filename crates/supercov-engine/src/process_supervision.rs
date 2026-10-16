//! Privacy-preserving child-process supervision for arbitrary test commands.

// Descriptor and watchdog handling reads the filesystem only on Unix; on
// Windows the module path is unused and the first Windows build said so.
#[cfg(unix)]
use std::fs;
use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicI32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use supercov_contracts::{
    COMMAND_TERMINATION_GRACE_MS, COMMAND_TIMEOUT_EXIT_CODE, DEFAULT_DIAGNOSTIC_INTERVAL_MS,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, FromRawFd, OwnedFd},
    unix::{ffi::OsStrExt as _, process::CommandExt as _},
};

#[derive(Debug)]
pub enum SupervisionError {
    InvalidMilliseconds {
        name: String,
    },
    EmptyCommand,
    Spawn {
        program: OsString,
        source: io::Error,
    },
    Wait(io::Error),
    Signal(io::Error),
    PlatformOperation {
        operation: &'static str,
        source: io::Error,
    },
    UnsupportedPlatform(&'static str),
}

impl std::fmt::Display for SupervisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMilliseconds { name } => {
                write!(
                    formatter,
                    "{name} must be a positive integer number of milliseconds"
                )
            }
            Self::EmptyCommand => write!(formatter, "test command must not be empty"),
            Self::Spawn { program, source } => {
                write!(
                    formatter,
                    "could not spawn {}: {source}",
                    program.to_string_lossy()
                )
            }
            Self::Wait(error) => write!(formatter, "could not wait for test command: {error}"),
            Self::Signal(error) => {
                write!(formatter, "could not install signal forwarding: {error}")
            }
            Self::PlatformOperation { operation, source } => {
                write!(formatter, "could not {operation}: {source}")
            }
            Self::UnsupportedPlatform(reason) => write!(
                formatter,
                "unsupported process supervision platform: {reason}"
            ),
        }
    }
}

impl std::error::Error for SupervisionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub cwd: PathBuf,
    /// `None` inherits the supervisor environment. `Some` clears it first and
    /// installs exactly these values.
    pub environment: Option<Vec<(OsString, OsString)>>,
    /// When set, stdout and stderr are merged into this newly-created file.
    /// The orchestration layer owns publication and cleanup of the file.
    pub captured_output: Option<PathBuf>,
    /// When set, what the command prints is passed on with these names put
    /// right. A stream that is a terminal is still the command's own.
    pub console: Option<ConsoleNames>,
}

/// How what a command prints names the project where it named Supercov's
/// copy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsoleNames {
    /// Each spelling of the copy's directory with the project's, longest
    /// first.
    pub places: Vec<(String, String)>,
    /// How a tool that names a file from where it runs reaches the project
    /// from the copy: `../../../../`.
    pub up_to_the_project: Option<String>,
}

impl ConsoleNames {
    /// One line as the command would have printed it in the project.
    pub fn line<'a>(&self, line: &'a str) -> std::borrow::Cow<'a, str> {
        let named = crate::workspace::project_named(line, &self.places);
        let text = named.as_deref().unwrap_or(line);
        let Some(up) = self.up_to_the_project.as_deref() else {
            return named.map_or(line.into(), Into::into);
        };
        let Some(relative) = relative_to_the_project(text, up) else {
            return named.map_or(line.into(), Into::into);
        };
        relative.into()
    }
}

/// A path written from the copy up to the project (`../../../../pkg/a.go`),
/// as it is written from the project: `pkg/a.go`, and `./a.go` for a file of
/// the directory itself, which is how Go writes one. Either separator.
fn relative_to_the_project(text: &str, up: &str) -> Option<String> {
    let other = up.replace('/', "\\");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some((at, prefix)) = [up, other.as_str()]
        .into_iter()
        .filter_map(|prefix| Some((rest.find(prefix)?, prefix)))
        .min_by_key(|(at, _)| *at)
    {
        let before = &rest[..at];
        let after = &rest[at + prefix.len()..];
        // Where a path begins, and not further up than the project.
        let begins = before.chars().next_back().is_none_or(|last| {
            !(last.is_alphanumeric() || matches!(last, '.' | '/' | '\\' | '_' | '-'))
        });
        if !begins || after.starts_with("..") {
            out.push_str(&rest[..at + prefix.len()]);
            rest = after;
            continue;
        }
        out.push_str(before);
        let name = after
            .split(|next: char| next == ':' || next.is_whitespace())
            .next()
            .unwrap_or_default();
        if !name.contains(['/', '\\']) {
            out.push('.');
            out.push_str(&prefix[prefix.len() - 1..]);
        }
        rest = after;
        changed = true;
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// The streams of a child whose output is passed on line by line.
struct Relays(Vec<std::sync::mpsc::Receiver<()>>);

/// Where each relayed stream is passed on to: this process's own output, by
/// a handle of its own. Rust's `stderr()` is one lock, the caller that
/// reports the run holds it for as long as the command runs, and a relay
/// writing through it waited for the command to end before its first line.
#[derive(Default)]
struct RelayTargets {
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
    /// Both streams go to one place (`2>&1`, a CI log), and the command
    /// writes both down this one pipe: two pipes read by two threads would
    /// pass its lines on in an order it did not write them in.
    both: Option<io::PipeReader>,
    /// Streams that are a terminal, each read from a terminal of its own.
    #[cfg(unix)]
    terminals: Vec<Terminal>,
}

/// A terminal the command writes to in place of the user's, so that what it
/// writes can be put right on the way and the command still sees a terminal:
/// its colours, its width and its progress lines stay as they were.
#[cfg(unix)]
struct Terminal {
    /// The end this process reads what the command wrote from.
    ours: std::fs::File,
    /// The user's terminal, where it is passed on to.
    real: std::fs::File,
}

#[cfg(unix)]
impl Terminal {
    /// A terminal of the size and settings of `real`, and the end of it the
    /// command is given.
    fn like(real: std::fs::File) -> Option<(Self, std::fs::File)> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: both structures are plain data the calls fill in, and the
        // descriptors `openpty` returns are owned by nothing else.
        unsafe {
            let mut settings: libc::termios = std::mem::zeroed();
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::tcgetattr(real.as_raw_fd(), &mut settings) != 0
                || libc::ioctl(real.as_raw_fd(), libc::TIOCGWINSZ, &mut size) != 0
            {
                return None;
            }
            // What the command writes is passed on byte for byte; the
            // user's terminal ends each line as it always did.
            settings.c_oflag &= !libc::OPOST;
            let (mut ours, mut theirs) = (-1, -1);
            // Raw pointers: Linux declares the last two as read only, macOS
            // does not.
            if libc::openpty(
                &mut ours,
                &mut theirs,
                std::ptr::null_mut(),
                &raw mut settings,
                &raw mut size,
            ) != 0
            {
                return None;
            }
            let ours = std::fs::File::from_raw_fd(ours);
            let theirs = std::fs::File::from_raw_fd(theirs);
            for end in [&ours, &theirs] {
                // Neither end is for a process this one starts, other than
                // as the stream `Command` makes of it.
                libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            }
            Some((Self { ours, real }, theirs))
        }
    }

    /// Gives the command's terminal the size the user's has now.
    fn resize(&self) {
        use std::os::fd::AsRawFd;
        // SAFETY: `size` is plain data the first call fills in.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(self.real.as_raw_fd(), libc::TIOCGWINSZ, &mut size) == 0 {
                libc::ioctl(self.ours.as_raw_fd(), libc::TIOCSWINSZ, &size);
            }
        }
    }

    /// Waits for the command to write, up to `wait`.
    fn written(&self, wait: Duration) -> bool {
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: self.ours.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one descriptor this process owns.
        unsafe { libc::poll(&mut poll, 1, wait.as_millis() as libc::c_int) > 0 }
    }
}

/// Whether two handles are one file, pipe or device.
#[cfg(unix)]
fn one_place(left: &std::fs::File, right: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (left.metadata(), right.metadata()) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn one_place(_left: &std::fs::File, _right: &std::fs::File) -> bool {
    false
}

#[cfg(unix)]
fn own_handle(stream: &impl std::os::fd::AsFd) -> Option<std::fs::File> {
    stream.as_fd().try_clone_to_owned().ok().map(Into::into)
}

#[cfg(windows)]
fn own_handle(stream: &impl std::os::windows::io::AsHandle) -> Option<std::fs::File> {
    stream.as_handle().try_clone_to_owned().ok().map(Into::into)
}

#[cfg(not(any(unix, windows)))]
fn own_handle<T>(_stream: &T) -> Option<std::fs::File> {
    None
}

/// How long a stream is still read after the command has ended. It ends with
/// the command unless a process the command left behind holds it open.
const RELAY_GRACE: Duration = Duration::from_secs(2);

impl Relays {
    /// Ask for a pipe on each stream that is not a terminal and can be
    /// written to by a handle of its own.
    fn request(command: &mut Command, names: Option<&ConsoleNames>) -> RelayTargets {
        use std::io::IsTerminal;
        let mut targets = RelayTargets::default();
        if names.is_none() {
            return targets;
        }
        if !io::stdout().is_terminal() {
            targets.stdout = own_handle(&io::stdout());
        }
        if !io::stderr().is_terminal() {
            targets.stderr = own_handle(&io::stderr());
        }
        #[cfg(unix)]
        {
            let terminal = |is: bool, handle: Option<std::fs::File>| handle.filter(|_| is);
            let stdout = terminal(io::stdout().is_terminal(), own_handle(&io::stdout()));
            let stderr = terminal(io::stderr().is_terminal(), own_handle(&io::stderr()));
            match (stdout, stderr) {
                // One terminal for both, so the command's lines keep their order.
                (Some(stdout), Some(stderr)) if one_place(&stdout, &stderr) => {
                    if let Some((terminal, theirs)) = Terminal::like(stdout)
                        && let Ok(second) = theirs.try_clone()
                    {
                        command.stdout(theirs).stderr(second);
                        targets.terminals.push(terminal);
                    }
                }
                (stdout, stderr) => {
                    if let Some((terminal, theirs)) = stdout.and_then(Terminal::like) {
                        command.stdout(theirs);
                        targets.terminals.push(terminal);
                    }
                    if let Some((terminal, theirs)) = stderr.and_then(Terminal::like) {
                        command.stderr(theirs);
                        targets.terminals.push(terminal);
                    }
                }
            }
        }
        if let (Some(stdout), Some(stderr)) = (&targets.stdout, &targets.stderr)
            && one_place(stdout, stderr)
            && let Ok((reader, writer)) = io::pipe()
            && let Ok(second) = writer.try_clone()
        {
            command.stdout(writer).stderr(second);
            targets.both = Some(reader);
            return targets;
        }
        if targets.stdout.is_some() {
            command.stdout(Stdio::piped());
        }
        if targets.stderr.is_some() {
            command.stderr(Stdio::piped());
        }
        targets
    }

    fn start(
        child: &mut std::process::Child,
        targets: RelayTargets,
        names: Option<&ConsoleNames>,
    ) -> Self {
        let mut relays = Vec::new();
        let Some(names) = names else {
            return Self(relays);
        };
        #[cfg(unix)]
        for terminal in targets.terminals {
            relays.push(relay_terminal(terminal, names.clone()));
        }
        if let Some(pipe) = targets.both {
            if let Some(to) = targets.stderr.or(targets.stdout) {
                relays.push(relay(pipe, to, names.clone()));
            }
            return Self(relays);
        }
        if let (Some(pipe), Some(to)) = (child.stdout.take(), targets.stdout) {
            relays.push(relay(pipe, to, names.clone()));
        }
        if let (Some(pipe), Some(to)) = (child.stderr.take(), targets.stderr) {
            relays.push(relay(pipe, to, names.clone()));
        }
        Self(relays)
    }

    fn finish(self) {
        for ended in self.0 {
            let _ = ended.recv_timeout(RELAY_GRACE);
        }
    }
}

/// A line longer than this is passed on in pieces.
const RELAY_LINE_LIMIT: usize = 64 * 1024;

fn relay(
    mut pipe: impl Read + Send + 'static,
    mut to: impl Write + Send + 'static,
    names: ConsoleNames,
) -> std::sync::mpsc::Receiver<()> {
    let (ended, wait) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut pass = |bytes: &[u8]| -> io::Result<()> {
            match std::str::from_utf8(bytes) {
                Ok(text) => to.write_all(names.line(text).as_bytes())?,
                Err(_) => to.write_all(bytes)?,
            }
            to.flush()
        };
        let mut pending = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            pending.extend_from_slice(&buffer[..read]);
            let mut passed = 0;
            while let Some(end) = pending[passed..].iter().position(|byte| *byte == b'\n') {
                // Whoever reads this has gone: the command is told so by
                // its next write, as it would have been.
                if pass(&pending[passed..=passed + end]).is_err() {
                    let _ = ended.send(());
                    return;
                }
                passed += end + 1;
            }
            pending.drain(..passed);
            if pending.len() > RELAY_LINE_LIMIT {
                if pass(&pending).is_err() {
                    let _ = ended.send(());
                    return;
                }
                pending.clear();
            }
        }
        if !pending.is_empty() {
            let _ = pass(&pending);
        }
        let _ = ended.send(());
    });
    wait
}

/// How long the end of a line is waited for once its beginning has come. A
/// prompt or a progress line has no end and is passed on after this; a name
/// is put right only in text that is passed on together, and a command
/// writes a line at once.
#[cfg(unix)]
const TERMINAL_LINE_WAIT: Duration = Duration::from_millis(40);

/// How often the command's terminal is given the size of the user's.
#[cfg(unix)]
const TERMINAL_SIZE_EVERY: Duration = Duration::from_millis(250);

/// Passes on what a command writes to its terminal as it writes it: whole
/// lines at once, and what is not yet a line after a moment.
#[cfg(unix)]
fn relay_terminal(mut terminal: Terminal, names: ConsoleNames) -> std::sync::mpsc::Receiver<()> {
    let (ended, wait) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let Ok(mut to) = terminal.real.try_clone() else {
            let _ = ended.send(());
            return;
        };
        let mut pass = |bytes: &[u8]| -> io::Result<()> {
            match std::str::from_utf8(bytes) {
                Ok(text) => to.write_all(names.line(text).as_bytes())?,
                Err(_) => to.write_all(bytes)?,
            }
            to.flush()
        };
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut sized = Instant::now();
        loop {
            let wait = if pending.is_empty() {
                TERMINAL_SIZE_EVERY
            } else {
                TERMINAL_LINE_WAIT
            };
            if !terminal.written(wait) {
                if !pending.is_empty() {
                    if pass(&pending).is_err() {
                        break;
                    }
                    pending.clear();
                }
                if sized.elapsed() >= TERMINAL_SIZE_EVERY {
                    terminal.resize();
                    sized = Instant::now();
                }
                continue;
            }
            // The last process holding the other end has gone: a read of
            // nothing on macOS, an error on Linux.
            let read = match terminal.ours.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            pending.extend_from_slice(&buffer[..read]);
            if let Some(end) = pending.iter().rposition(|byte| *byte == b'\n') {
                if pass(&pending[..=end]).is_err() {
                    break;
                }
                pending.drain(..=end);
            }
            if pending.len() > RELAY_LINE_LIMIT {
                if pass(&pending).is_err() {
                    break;
                }
                pending.clear();
            }
        }
        if !pending.is_empty() {
            let _ = pass(&pending);
        }
        let _ = ended.send(());
    });
    wait
}

impl CommandSpec {
    pub fn command(&self) -> Result<Command, SupervisionError> {
        if self.program.is_empty() {
            return Err(SupervisionError::EmptyCommand);
        }
        let mut command = Command::new(self.resolved_program());
        command
            .args(&self.arguments)
            .current_dir(&self.cwd)
            .stdin(Stdio::inherit());
        if let Some(path) = &self.captured_output {
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|source| SupervisionError::PlatformOperation {
                    operation: "create captured process output",
                    source,
                })?;
            let errors =
                output
                    .try_clone()
                    .map_err(|source| SupervisionError::PlatformOperation {
                        operation: "clone captured process output",
                        source,
                    })?;
            command
                .stdout(Stdio::from(output))
                .stderr(Stdio::from(errors));
        } else {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
        if let Some(environment) = &self.environment {
            command.env_clear().envs(environment.iter().cloned());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED,
            };
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
        }
        Ok(command)
    }
}

impl CommandSpec {
    /// Windows finds `foo.exe` for a bare `foo` and nothing else, while a
    /// gem's or npm's executable there is a `.bat` or `.cmd` shim: `rspec`
    /// failed with "program not found" with `rspec.bat` sitting on PATH.
    /// Resolve the program the way the shell does -- every PATH entry, every
    /// PATHEXT extension -- so the standard library sees the extension and
    /// runs a batch file through cmd.exe with its own quoting.
    #[cfg(windows)]
    fn resolved_program(&self) -> OsString {
        // Variable names are case-insensitive there, and PATH is usually
        // spelled `Path`.
        let variable = |name: &str| match &self.environment {
            Some(environment) => environment
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone()),
            None => std::env::var_os(name),
        };
        resolve_program_with(
            &self.program,
            &self.cwd,
            variable("PATH"),
            variable("PATHEXT"),
        )
    }

    #[cfg(not(windows))]
    fn resolved_program(&self) -> OsString {
        self.program.clone()
    }
}

/// The host-independent half of program resolution, so it is tested on every
/// host. A bare name is looked for in each PATH directory, a name with a
/// separator relative to the working directory; in either place a candidate
/// carrying a PATHEXT extension wins over the bare file, as in cmd.exe,
/// because an extensionless script cannot be started on Windows even when it
/// exists. A program nothing matches is returned unchanged so the operating
/// system reports the failure in its own words.
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_program_with(
    program: &OsStr,
    cwd: &Path,
    path: Option<OsString>,
    pathext: Option<OsString>,
) -> OsString {
    const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";
    let pathext = pathext
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_PATHEXT.into());
    let extensions = pathext
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let directories = if program.to_string_lossy().contains(['\\', '/']) {
        vec![cwd.to_path_buf()]
    } else {
        path.map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default()
    };
    let is_file = |candidate: &Path| std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file());
    for directory in directories {
        let base = directory.join(program);
        for extension in &extensions {
            let mut candidate = base.clone().into_os_string();
            candidate.push(extension);
            if is_file(Path::new(&candidate)) {
                return candidate;
            }
        }
        if base.extension().is_some() && is_file(&base) {
            return base.into_os_string();
        }
    }
    program.to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionOptions {
    pub diagnostic_interval: Duration,
    pub timeout: Option<Duration>,
    pub termination_grace: Duration,
}

impl Default for SupervisionOptions {
    fn default() -> Self {
        Self {
            diagnostic_interval: Duration::from_millis(DEFAULT_DIAGNOSTIC_INTERVAL_MS),
            timeout: None,
            termination_grace: Duration::from_millis(COMMAND_TERMINATION_GRACE_MS),
        }
    }
}

impl SupervisionOptions {
    pub fn from_environment() -> Result<Self, SupervisionError> {
        Ok(Self {
            diagnostic_interval: positive_milliseconds(
                std::env::var("SUPERCOV_DIAGNOSTIC_INTERVAL_MS")
                    .ok()
                    .as_deref(),
                "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
            )?
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_DIAGNOSTIC_INTERVAL_MS)),
            timeout: positive_milliseconds(
                std::env::var("SUPERCOV_COMMAND_TIMEOUT_MS").ok().as_deref(),
                "SUPERCOV_COMMAND_TIMEOUT_MS",
            )?,
            termination_grace: Duration::from_millis(COMMAND_TERMINATION_GRACE_MS),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_tenths: Option<u64>,
    pub executable: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ForwardedSignal {
    Sighup,
    Sigint,
    Sigterm,
}

impl ForwardedSignal {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Sighup => 129,
            Self::Sigint => 130,
            Self::Sigterm => 143,
        }
    }

    #[cfg(unix)]
    fn raw(self) -> i32 {
        match self {
            Self::Sighup => libc::SIGHUP,
            Self::Sigint => libc::SIGINT,
            Self::Sigterm => libc::SIGTERM,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisedResult {
    pub status: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub interrupted_signal: Option<ForwardedSignal>,
}

#[derive(Debug)]
pub struct SupervisedOutput {
    pub result: SupervisedResult,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl SupervisedResult {
    pub fn exit_code(&self) -> i32 {
        if self.timed_out {
            COMMAND_TIMEOUT_EXIT_CODE
        } else if let Some(signal) = self.interrupted_signal {
            signal.exit_code()
        } else {
            self.status.unwrap_or(128)
        }
    }
}

pub fn positive_milliseconds(
    value: Option<&str>,
    name: &str,
) -> Result<Option<Duration>, SupervisionError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let milliseconds = value
        .parse::<u64>()
        .ok()
        .filter(|milliseconds| *milliseconds > 0)
        .ok_or_else(|| SupervisionError::InvalidMilliseconds { name: name.into() })?;
    Ok(Some(Duration::from_millis(milliseconds)))
}

fn process_inventory() -> Vec<ProcessSnapshot> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_exe(UpdateKind::OnlyIfNotSet),
        ),
    );
    system
        .processes()
        .iter()
        // Linux lists a process's threads beside it as tasks of their own;
        // a thread keeps no command running that its process does not.
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| ProcessSnapshot {
            pid: pid.as_u32(),
            parent_pid: process.parent().map_or(0, sysinfo::Pid::as_u32),
            state: Some(process_status(process.status()).into()),
            cpu_tenths: Some(process.accumulated_cpu_time() / 100),
            // Named by what it runs: a process's own name is its main
            // thread's, which Node renames `MainThread` on Linux.
            executable: process
                .exe()
                .and_then(Path::file_name)
                .or_else(|| Path::new(process.name()).file_name())
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_owned(),
        })
        .collect()
}

fn process_status(status: sysinfo::ProcessStatus) -> &'static str {
    use sysinfo::ProcessStatus;
    match status {
        ProcessStatus::Idle => "I",
        ProcessStatus::Run => "R",
        ProcessStatus::Sleep => "S",
        ProcessStatus::Stop => "T",
        ProcessStatus::Zombie => "Z",
        ProcessStatus::Tracing => "t",
        ProcessStatus::Dead => "X",
        ProcessStatus::Wakekill => "K",
        ProcessStatus::Waking => "W",
        ProcessStatus::Parked => "P",
        ProcessStatus::LockBlocked => "L",
        ProcessStatus::UninterruptibleDiskSleep => "D",
        ProcessStatus::Suspended => "S",
        ProcessStatus::Unknown(_) => "?",
    }
}

pub fn descendant_process_tree(root_pid: u32) -> Vec<ProcessSnapshot> {
    let inventory = process_inventory();
    let mut descendants = std::collections::BTreeSet::from([root_pid]);
    loop {
        let before = descendants.len();
        for process in &inventory {
            if descendants.contains(&process.parent_pid) {
                descendants.insert(process.pid);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    let mut result = inventory
        .into_iter()
        .filter(|process| descendants.contains(&process.pid))
        .collect::<Vec<_>>();
    result.sort_by_key(|process| process.pid);
    result
}

fn format_duration(milliseconds: u128) -> String {
    if milliseconds < 1_000 {
        return format!("{milliseconds}ms");
    }
    let seconds = (milliseconds + 500) / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

pub fn format_process_diagnostic(
    root_pid: u32,
    elapsed: Duration,
    tree: &[ProcessSnapshot],
) -> String {
    let mut output = format!(
        "[supercov] command still running after {}",
        format_duration(elapsed.as_millis())
    );
    if tree.is_empty() {
        output.push_str(&format!("\n  pid={root_pid} process details unavailable"));
        return output;
    }
    for process in tree {
        output.push_str(&format!(
            "\n  pid={} ppid={} exe={}",
            process.pid, process.parent_pid, process.executable
        ));
        if let Some(state) = &process.state {
            output.push_str(&format!(" state={state}"));
        }
        if let Some(cpu_tenths) = process.cpu_tenths {
            output.push_str(&format!(" cpu={}.{}s", cpu_tenths / 10, cpu_tenths % 10));
        }
    }
    output
}

#[cfg(unix)]
struct SignalFlags {
    _exclusive: MutexGuard<'static, ()>,
    previous: Vec<(i32, libc::sigaction)>,
}

#[cfg(unix)]
impl SignalFlags {
    fn install() -> Result<Self, SupervisionError> {
        let exclusive = SIGNAL_HANDLER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RECEIVED_SIGNAL.store(0, Ordering::SeqCst);
        let mut previous = Vec::new();
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            // SAFETY: zero is a valid initial state for `sigaction`; every
            // field used by the kernel is initialized below before the call.
            let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
            action.sa_sigaction = record_signal as *const () as usize;
            // SAFETY: `action.sa_mask` is a valid, writable signal set.
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            action.sa_flags = 0;
            // SAFETY: `old` is initialized by a successful `sigaction` call.
            let mut old = unsafe { std::mem::zeroed::<libc::sigaction>() };
            // SAFETY: pointers reference live `sigaction` values and the
            // signal is one of the three catchable POSIX signals above.
            if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                for (installed, old) in previous.iter().rev() {
                    // SAFETY: restores a handler returned by `sigaction`.
                    let _ = unsafe { libc::sigaction(*installed, old, std::ptr::null_mut()) };
                }
                return Err(SupervisionError::Signal(io::Error::last_os_error()));
            }
            previous.push((signal, old));
        }
        Ok(Self {
            _exclusive: exclusive,
            previous,
        })
    }

    fn received(&self) -> Option<ForwardedSignal> {
        // A supervisor is shared by every concurrently running child in one
        // execution session. Keep the signal visible until the session guard
        // is dropped so every process group observes the same interruption.
        match RECEIVED_SIGNAL.load(Ordering::SeqCst) {
            libc::SIGHUP => Some(ForwardedSignal::Sighup),
            libc::SIGINT => Some(ForwardedSignal::Sigint),
            libc::SIGTERM => Some(ForwardedSignal::Sigterm),
            _ => None,
        }
    }
}

#[cfg(unix)]
impl Drop for SignalFlags {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.drain(..).rev() {
            // SAFETY: `previous` came directly from a successful `sigaction`
            // call for the same signal and remains live for this call.
            let _ = unsafe { libc::sigaction(signal, &previous, std::ptr::null_mut()) };
        }
        RECEIVED_SIGNAL.store(0, Ordering::SeqCst);
    }
}

#[cfg(unix)]
static SIGNAL_HANDLER_LOCK: Mutex<()> = Mutex::new(());
#[cfg(unix)]
static RECEIVED_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record_signal(signal: i32) {
    RECEIVED_SIGNAL.store(signal, Ordering::SeqCst);
}

#[cfg(windows)]
struct SignalFlags {
    _exclusive: MutexGuard<'static, ()>,
}

#[cfg(windows)]
impl SignalFlags {
    fn install() -> Result<Self, SupervisionError> {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

        let exclusive = SIGNAL_HANDLER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RECEIVED_SIGNAL.store(0, Ordering::SeqCst);
        // SAFETY: `record_console_signal` has the required system ABI and
        // remains installed only while this guard is alive.
        if unsafe { SetConsoleCtrlHandler(Some(record_console_signal), 1) } == 0 {
            return Err(SupervisionError::Signal(io::Error::last_os_error()));
        }
        Ok(Self {
            _exclusive: exclusive,
        })
    }

    fn received(&self) -> Option<ForwardedSignal> {
        match RECEIVED_SIGNAL.load(Ordering::SeqCst) {
            2 => Some(ForwardedSignal::Sigint),
            15 => Some(ForwardedSignal::Sigterm),
            _ => None,
        }
    }
}

#[cfg(windows)]
impl Drop for SignalFlags {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

        // SAFETY: removes exactly the handler installed by `install`.
        let _ = unsafe { SetConsoleCtrlHandler(Some(record_console_signal), 0) };
        RECEIVED_SIGNAL.store(0, Ordering::SeqCst);
    }
}

#[cfg(windows)]
static SIGNAL_HANDLER_LOCK: Mutex<()> = Mutex::new(());
#[cfg(windows)]
static RECEIVED_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[cfg(windows)]
unsafe extern "system" fn record_console_signal(control: u32) -> i32 {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    match control {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            RECEIVED_SIGNAL.store(2, Ordering::SeqCst);
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            RECEIVED_SIGNAL.store(15, Ordering::SeqCst);
            1
        }
        _ => 0,
    }
}

#[cfg(windows)]
pub(crate) struct JobHandle(windows_sys::Win32::Foundation::HANDLE);

// A job-object handle is a process-wide kernel token, not a pointer into this
// thread's memory: assigning a process to it, terminating it and closing it are
// all safe from any thread, which is the same invariant std's OwnedHandle
// carries by being Send and Sync. Without these the raw HANDLE makes the whole
// supervisor !Sync on Windows, and the Rust test runner, which shares one
// supervisor across scoped threads, does not compile there -- the first
// Windows build found exactly that.
#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}

#[cfg(windows)]
impl JobHandle {
    pub(crate) fn new() -> Result<Self, SupervisionError> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: null security attributes and name create one private job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(SupervisionError::PlatformOperation {
                operation: "create a Windows Job Object",
                source: io::Error::last_os_error(),
            });
        }
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the buffer is a live value of the exact information class
        // and length requested by SetInformationJobObject.
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(SupervisionError::PlatformOperation {
                operation: "configure Windows Job Object containment",
                source: io::Error::last_os_error(),
            });
        }
        Ok(job)
    }

    pub(crate) fn assign(&self, child: &Child) -> Result<(), SupervisionError> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        // SAFETY: Child owns a live process handle with the rights granted by
        // CreateProcess; the job handle stays live for the complete plan.
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle().cast()) } == 0 {
            return Err(SupervisionError::PlatformOperation {
                operation: "assign the suspended command to its Windows Job Object",
                source: io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the handle owns this invocation's process tree. Failure can
        // only mean the tree has already exited, so termination is best effort.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE makes this the final, crash-safe
        // containment boundary for descendants that outlive their root.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn resume_suspended_process(pid: u32) -> Result<(), SupervisionError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };

    // The stdlib exposes the process handle but not CreateProcess's primary
    // thread handle. Starting suspended, assigning the job, then resuming the
    // process-owned thread from a ToolHelp snapshot closes the escape race.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(SupervisionError::PlatformOperation {
            operation: "enumerate the suspended command threads",
            source: io::Error::last_os_error(),
        });
    }
    struct Snapshot(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Snapshot {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let _snapshot = Snapshot(snapshot);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot, &raw mut entry) } == 0 {
        return Err(SupervisionError::PlatformOperation {
            operation: "read the suspended command thread snapshot",
            source: io::Error::last_os_error(),
        });
    }
    loop {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(SupervisionError::PlatformOperation {
                    operation: "open the suspended command's primary thread",
                    source: io::Error::last_os_error(),
                });
            }
            // SAFETY: the handle identifies a suspended thread owned by the
            // just-created process and is closed immediately after resuming.
            let resumed = unsafe { ResumeThread(thread) };
            let resume_error = (resumed == u32::MAX).then(io::Error::last_os_error);
            let _ = unsafe { CloseHandle(thread) };
            if let Some(source) = resume_error {
                return Err(SupervisionError::PlatformOperation {
                    operation: "resume the contained command",
                    source,
                });
            }
            return Ok(());
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if unsafe { Thread32Next(snapshot, &raw mut entry) } == 0 {
            break;
        }
    }
    Err(SupervisionError::PlatformOperation {
        operation: "locate the suspended command's primary thread",
        source: io::Error::new(io::ErrorKind::NotFound, "process thread was absent"),
    })
}

#[cfg(windows)]
fn forward_windows_control(child: &Child) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // CREATE_NEW_PROCESS_GROUP makes the child's PID its console group ID.
    // Some non-console commands reject the event; the grace-period Job Object
    // termination remains authoritative in that case.
    let _ = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
}

/// The write end is held only by the supervising process. A tiny watchdog
/// created in the command's pre-exec child blocks on the read end. Normal
/// completion, an unwind, or uncatchable supervisor death all close this
/// descriptor and make the watchdog kill the command's complete process group.
#[cfg(unix)]
struct ParentDeathGuard {
    _writer: OwnedFd,
}

#[cfg(unix)]
fn parent_death_pipe() -> Result<(OwnedFd, OwnedFd), SupervisionError> {
    let mut descriptors = [-1_i32; 2];
    // SAFETY: `descriptors` is a live two-element output buffer for pipe(2).
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(SupervisionError::PlatformOperation {
            operation: "create parent-death supervision pipe",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: pipe(2) returned two newly owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    // SAFETY: same as above for the write end.
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    for descriptor in [read.as_raw_fd(), write.as_raw_fd()] {
        // SAFETY: the descriptor is live and F_SETFD accepts FD_CLOEXEC.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(SupervisionError::PlatformOperation {
                operation: "protect parent-death supervision pipe across exec",
                source: io::Error::last_os_error(),
            });
        }
    }
    Ok((read, write))
}

#[cfg(unix)]
fn spawn_contained(
    command: &mut Command,
    program: &OsString,
    watchdog_program: Option<&Path>,
) -> Result<(Child, Option<ParentDeathGuard>), SupervisionError> {
    let Some(watchdog_program) = watchdog_program else {
        command.process_group(0);
        let child = command.spawn().map_err(|source| SupervisionError::Spawn {
            program: program.clone(),
            source,
        })?;
        return Ok((child, None));
    };
    let (read, write) = parent_death_pipe()?;
    let (ready_read, ready_write) = parent_death_pipe()?;
    let read_descriptor = read.as_raw_fd();
    let write_descriptor = write.as_raw_fd();
    let ready_read_descriptor = ready_read.as_raw_fd();
    let ready_write_descriptor = ready_write.as_raw_fd();
    let watchdog_program = std::ffi::CString::new(watchdog_program.as_os_str().as_bytes())
        .map_err(|_| SupervisionError::PlatformOperation {
            operation: "encode parent-death watchdog executable",
            source: io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"),
        })?;
    let watchdog_argument =
        std::ffi::CString::new("__watch-process-group").expect("static CString");
    // SAFETY: this closure calls only async-signal-safe syscalls between fork
    // and exec. The forked watchdog immediately execs the already-loaded
    // Supercov binary; it never returns to Rust or touches an inherited lock.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let _ = libc::close(write_descriptor);
            let watchdog = libc::fork();
            if watchdog < 0 {
                return Err(io::Error::last_os_error());
            }
            if watchdog == 0 {
                let _ = libc::close(ready_read_descriptor);
                if libc::dup2(read_descriptor, 0) < 0 || libc::dup2(ready_write_descriptor, 3) < 0 {
                    libc::_exit(125);
                }
                for descriptor in [read_descriptor, ready_write_descriptor, 1, 2] {
                    if descriptor != 0 && descriptor != 3 {
                        let _ = libc::close(descriptor);
                    }
                }
                let arguments = [
                    watchdog_program.as_ptr(),
                    watchdog_argument.as_ptr(),
                    std::ptr::null(),
                ];
                libc::execv(watchdog_program.as_ptr(), arguments.as_ptr());
                libc::_exit(125);
            }
            let _ = libc::close(read_descriptor);
            let _ = libc::close(ready_write_descriptor);
            let mut ready = 0_u8;
            loop {
                let received = libc::read(ready_read_descriptor, (&raw mut ready).cast(), 1);
                if received == 1 && ready == 1 {
                    break;
                }
                if received < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(io::Error::other(
                    "parent-death watchdog failed before command exec",
                ));
            }
            let _ = libc::close(ready_read_descriptor);
            Ok(())
        });
    }
    let child = command.spawn().map_err(|source| SupervisionError::Spawn {
        program: program.clone(),
        source,
    })?;
    drop(read);
    drop(ready_read);
    drop(ready_write);
    Ok((child, Some(ParentDeathGuard { _writer: write })))
}

#[cfg(unix)]
pub fn watch_parent_process_group() -> io::Result<()> {
    // The target waits for our readiness byte, so its PID remains both our
    // parent PID and the process-group ID until containment is armed.
    let process_group = unsafe { libc::getppid() };
    if process_group <= 1 {
        return Err(io::Error::other(
            "parent-death watchdog has no target process",
        ));
    }
    // SAFETY: the watchdog is a non-leader child in the target's process group.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor_root = if Path::new("/proc/self/fd").is_dir() {
        Path::new("/proc/self/fd")
    } else {
        Path::new("/dev/fd")
    };
    let descriptors = fs::read_dir(descriptor_root)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i32>().ok())
        .filter(|descriptor| !matches!(*descriptor, 0 | 3))
        .collect::<Vec<_>>();
    for descriptor in descriptors {
        // SAFETY: closing a descriptor that the directory iterator already
        // released, or one concurrently absent, is harmless.
        let _ = unsafe { libc::close(descriptor) };
    }
    let ready = [1_u8];
    // SAFETY: pre-exec mapped the private readiness pipe to descriptor 3.
    if unsafe { libc::write(3, ready.as_ptr().cast(), ready.len()) } != 1 {
        return Err(io::Error::last_os_error());
    }
    let _ = unsafe { libc::close(3) };
    let mut buffer = [0_u8; 1];
    loop {
        // The supervisor never writes. EOF means normal supervisor teardown,
        // unwind, or uncatchable process death.
        let read = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read == 0 {
            break;
        }
        if read < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
    }
    let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    Ok(())
}

#[cfg(not(unix))]
pub fn watch_parent_process_group() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the POSIX parent-death watchdog is unavailable",
    ))
}

/// The terminal, lent to a command that reads the keyboard.
///
/// A command runs in a process group of its own, so that all of it can be
/// ended, and the terminal belongs to Supercov's group: a read of the
/// keyboard (a debugger at a breakpoint, a watch mode, a question) stops the
/// command where it stands, as it stops a job a shell put in the background.
/// When that happens the command's group is given the terminal and
/// continued. Until then Ctrl-C reaches Supercov as it always has; after, it
/// reaches the command, which is what someone at a debugger means by it.
#[cfg(unix)]
#[derive(Default)]
struct Keyboard {
    terminal: Option<fs::File>,
    lent: bool,
}

#[cfg(unix)]
impl Keyboard {
    /// The signal the command stopped on, if it has stopped since last asked.
    fn stopped(child: &Child) -> Option<i32> {
        // SAFETY: `info` is plain data the call fills in. Asking for stops
        // alone leaves the command's exit for `try_wait` to collect.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WSTOPPED | libc::WNOHANG,
            ) != 0
                || info.si_code != libc::CLD_STOPPED
            {
                return None;
            }
            Some(info.si_status())
        }
    }

    /// Makes `group` the one the terminal's keys and signals go to. False
    /// when there is no terminal or it is not this process's to give.
    fn give(&mut self, group: libc::pid_t, ours_to_give: bool) -> bool {
        use std::os::fd::AsRawFd;
        if self.terminal.is_none() {
            self.terminal = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok();
        }
        let Some(terminal) = &self.terminal else {
            return false;
        };
        // SAFETY: plain calls on a descriptor this process owns. Setting the
        // group from one that is not the terminal's stops the caller unless
        // the signal that would do it is held back for the moment.
        unsafe {
            if ours_to_give && libc::tcgetpgrp(terminal.as_raw_fd()) != libc::getpgrp() {
                return false;
            }
            let mut held: libc::sigset_t = std::mem::zeroed();
            let mut before: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut held);
            libc::sigaddset(&mut held, libc::SIGTTOU);
            libc::pthread_sigmask(libc::SIG_BLOCK, &held, &mut before);
            let given = libc::tcsetpgrp(terminal.as_raw_fd(), group) == 0;
            libc::pthread_sigmask(libc::SIG_SETMASK, &before, std::ptr::null_mut());
            given
        }
    }

    /// The command stopped on `signal`. Returns whether it was continued.
    fn command_stopped(&mut self, child: &mut Child, signal: i32) -> bool {
        let group = child.id() as libc::pid_t;
        match signal {
            libc::SIGTTIN | libc::SIGTTOU if !self.lent => {
                if !self.give(group, true) {
                    // Supercov is itself in the background: the command
                    // waits for the terminal as Supercov would.
                    return false;
                }
                self.lent = true;
            }
            // Ctrl-Z, with the terminal lent: it stopped the command and
            // not Supercov, and the shell is waiting on Supercov. Stop too,
            // and lend the terminal again when the shell continues us.
            libc::SIGTSTP if self.lent => {
                self.take_back();
                // SAFETY: stops this process until it is continued.
                unsafe { libc::raise(libc::SIGSTOP) };
                self.lent = self.give(group, true);
            }
            _ => return false,
        }
        signal_process_group(child, libc::SIGCONT);
        true
    }

    fn take_back(&mut self) {
        if std::mem::take(&mut self.lent) {
            // SAFETY: reads this process's own group.
            let ours = unsafe { libc::getpgrp() };
            self.give(ours, false);
        }
    }
}

#[cfg(unix)]
impl Drop for Keyboard {
    fn drop(&mut self) {
        self.take_back();
    }
}

#[cfg(unix)]
fn signal_process_group(child: &mut Child, signal: i32) {
    let pid = child.id() as i32;
    // SAFETY: `kill` is async-signal-safe and receives a process-group ID
    // created for this child before exec. Failure can mean the child exited
    // between `try_wait` and this call, so it is intentionally non-fatal.
    let group_result = unsafe { libc::kill(-pid, signal) };
    if group_result != 0 {
        // SAFETY: same rationale, with the child PID as a last-resort target.
        let _ = unsafe { libc::kill(pid, signal) };
    }
}

#[cfg(unix)]
fn exit_parts(status: ExitStatus) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    (status.code(), status.signal())
}

#[cfg(not(unix))]
fn exit_parts(status: ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), None)
}

/// One diagnostic line in one write. `writeln!` with arguments writes the
/// pieces between them separately, and the command being supervised shares
/// this stderr: a child printing at the same moment landed inside the timeout
/// line, splitting it where the number goes. A single write of a short line to
/// a pipe cannot be interleaved.
fn write_line(writer: &mut dyn Write, line: &str) -> std::io::Result<()> {
    let mut bytes = String::with_capacity(line.len() + 1);
    bytes.push_str(line);
    bytes.push('\n');
    writer.write_all(bytes.as_bytes())?;
    writer.flush()
}

fn write_diagnostic(child: &Child, started: Instant, writer: &mut dyn Write) {
    let tree = descendant_process_tree(child.id());
    let diagnostic = format_process_diagnostic(child.id(), started.elapsed(), &tree);
    let verbose = std::env::var("SUPERCOV_VERBOSE")
        .or_else(|_| std::env::var("SUPERCOV_DEBUG"))
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    let diagnostic = if verbose {
        diagnostic.as_str()
    } else {
        diagnostic.lines().next().unwrap_or(diagnostic.as_str())
    };
    let _ = writeln!(writer, "{}", diagnostic).and_then(|_| writer.flush());
}

fn validate_options(options: SupervisionOptions) -> Result<(), SupervisionError> {
    if options.diagnostic_interval.is_zero() || options.termination_grace.is_zero() {
        return Err(SupervisionError::InvalidMilliseconds {
            name: "process supervision interval".into(),
        });
    }
    if options.timeout.is_some_and(|timeout| timeout.is_zero()) {
        return Err(SupervisionError::InvalidMilliseconds {
            name: "SUPERCOV_COMMAND_TIMEOUT_MS".into(),
        });
    }
    Ok(())
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn captured_bytes(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: &'static str,
) -> Result<Vec<u8>, SupervisionError> {
    reader
        .join()
        .map_err(|_| SupervisionError::PlatformOperation {
            operation: "join captured process output reader",
            source: io::Error::other(format!("{stream} reader panicked")),
        })?
        .map_err(|source| SupervisionError::PlatformOperation {
            operation: "read captured process output",
            source,
        })
}

#[cfg(unix)]
pub struct ProcessSupervisor {
    signals: SignalFlags,
    watchdog_program: Option<PathBuf>,
}

#[cfg(unix)]
impl ProcessSupervisor {
    pub fn new() -> Result<Self, SupervisionError> {
        Ok(Self {
            signals: SignalFlags::install()?,
            watchdog_program: None,
        })
    }

    pub fn new_crash_safe(watchdog_program: &Path) -> Result<Self, SupervisionError> {
        let watchdog_program = fs::canonicalize(watchdog_program).map_err(|source| {
            SupervisionError::PlatformOperation {
                operation: "resolve parent-death watchdog executable",
                source,
            }
        })?;
        if !fs::metadata(&watchdog_program).is_ok_and(|metadata| metadata.is_file()) {
            return Err(SupervisionError::PlatformOperation {
                operation: "validate parent-death watchdog executable",
                source: io::Error::new(io::ErrorKind::InvalidInput, "expected a regular file"),
            });
        }
        Ok(Self {
            signals: SignalFlags::install()?,
            watchdog_program: Some(watchdog_program),
        })
    }

    pub fn supervise(
        &self,
        spec: &CommandSpec,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        validate_options(options)?;
        if let Some(signal) = self.signals.received() {
            return Ok(SupervisedResult {
                status: None,
                signal: Some(signal.raw()),
                timed_out: false,
                interrupted_signal: Some(signal),
            });
        }
        let mut command = spec.command()?;
        let targets = Relays::request(&mut command, spec.console.as_ref());
        let (mut child, _parent_death_guard) = spawn_contained(
            &mut command,
            &spec.program,
            self.watchdog_program.as_deref(),
        )?;
        // This process's ends of a pipe the child was given go with the
        // command; held, the pipe would never end.
        drop(command);
        let relays = Relays::start(&mut child, targets, spec.console.as_ref());
        let result = self.monitor(&mut child, options, writer);
        relays.finish();
        result
    }

    pub fn supervise_captured(
        &self,
        spec: &CommandSpec,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedOutput, SupervisionError> {
        validate_options(options)?;
        if spec.captured_output.is_some() {
            return Err(SupervisionError::PlatformOperation {
                operation: "configure separate captured process output",
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "merged and separate capture cannot be requested together",
                ),
            });
        }
        if let Some(signal) = self.signals.received() {
            return Ok(SupervisedOutput {
                result: SupervisedResult {
                    status: None,
                    signal: Some(signal.raw()),
                    timed_out: false,
                    interrupted_signal: Some(signal),
                },
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        let mut command = spec.command()?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let (mut child, parent_death_guard) = spawn_contained(
            &mut command,
            &spec.program,
            self.watchdog_program.as_deref(),
        )?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_reader = thread::spawn(move || read_pipe(stdout));
        let stderr_reader = thread::spawn(move || read_pipe(stderr));
        let result = self.monitor(&mut child, options, writer);
        // Closing the liveness writer makes the watchdog kill any descendants
        // that retained the output pipes after the root command exited.
        drop(parent_death_guard);
        let stdout = captured_bytes(stdout_reader, "stdout")?;
        let stderr = captured_bytes(stderr_reader, "stderr")?;
        Ok(SupervisedOutput {
            result: result?,
            stdout,
            stderr,
        })
    }

    fn monitor(
        &self,
        child: &mut Child,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        let started = Instant::now();
        let mut next_diagnostic = started + options.diagnostic_interval;
        let timeout_at = options.timeout.map(|timeout| started + timeout);
        let mut termination: Option<(Instant, Option<ForwardedSignal>)> = None;
        let mut timed_out = false;
        let mut interrupted_signal = None;
        let mut escalated = false;
        let mut keyboard = Keyboard::default();

        loop {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    signal_process_group(child, libc::SIGKILL);
                    let _ = child.wait();
                    return Err(SupervisionError::Wait(error));
                }
            };
            if let Some(status) = status {
                let (status, signal) = exit_parts(status);
                // With the terminal lent, Ctrl-C went to the command alone.
                // A command it ended is a run that was interrupted, as it
                // is when Supercov is the one told.
                if keyboard.lent
                    && interrupted_signal.is_none()
                    && !timed_out
                    && (signal == Some(libc::SIGINT) || status == Some(128 + libc::SIGINT))
                {
                    interrupted_signal = Some(ForwardedSignal::Sigint);
                }
                keyboard.take_back();
                return Ok(SupervisedResult {
                    status,
                    signal,
                    timed_out,
                    interrupted_signal,
                });
            }
            if termination.is_none()
                && let Some(signal) = Keyboard::stopped(child)
            {
                keyboard.command_stopped(child, signal);
            }
            let now = Instant::now();
            if termination.is_none()
                && let Some(signal) = self.signals.received()
            {
                interrupted_signal = Some(signal);
                signal_process_group(child, signal.raw());
                termination = Some((now, Some(signal)));
            }
            if termination.is_none() && timeout_at.is_some_and(|deadline| now >= deadline) {
                timed_out = true;
                let _ = write_line(
                    writer,
                    &format!(
                        "[supercov] command exceeded SUPERCOV_COMMAND_TIMEOUT_MS={}; terminating process group",
                        options.timeout.expect("timeout deadline").as_millis()
                    ),
                );
                signal_process_group(child, libc::SIGTERM);
                termination = Some((now, None));
                write_diagnostic(child, started, writer);
            }
            if now >= next_diagnostic && !timed_out {
                write_diagnostic(child, started, writer);
                while next_diagnostic <= now {
                    next_diagnostic += options.diagnostic_interval;
                }
            }
            if !escalated
                && termination.is_some_and(|(terminated_at, _)| {
                    now.duration_since(terminated_at) >= options.termination_grace
                })
            {
                signal_process_group(child, libc::SIGKILL);
                escalated = true;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(windows)]
pub struct ProcessSupervisor {
    signals: SignalFlags,
    job: JobHandle,
}

#[cfg(windows)]
impl ProcessSupervisor {
    pub fn new() -> Result<Self, SupervisionError> {
        Ok(Self {
            signals: SignalFlags::install()?,
            job: JobHandle::new()?,
        })
    }

    pub fn new_crash_safe(_watchdog_program: &Path) -> Result<Self, SupervisionError> {
        Self::new()
    }

    pub fn supervise(
        &self,
        spec: &CommandSpec,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        validate_options(options)?;
        if let Some(signal) = self.signals.received() {
            return Ok(SupervisedResult {
                status: None,
                signal: None,
                timed_out: false,
                interrupted_signal: Some(signal),
            });
        }
        let mut command = spec.command()?;
        let targets = Relays::request(&mut command, spec.console.as_ref());
        let mut child = command.spawn().map_err(|source| SupervisionError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        if let Err(error) = self.job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            self.job.terminate();
            let _ = child.wait();
            return Err(error);
        }
        // This process's ends of a pipe the child was given go with the
        // command; held, the pipe would never end.
        drop(command);
        let relays = Relays::start(&mut child, targets, spec.console.as_ref());
        let result = self.monitor(&mut child, options, writer);
        relays.finish();
        result
    }

    pub fn supervise_captured(
        &self,
        spec: &CommandSpec,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedOutput, SupervisionError> {
        validate_options(options)?;
        if spec.captured_output.is_some() {
            return Err(SupervisionError::PlatformOperation {
                operation: "configure separate captured process output",
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "merged and separate capture cannot be requested together",
                ),
            });
        }
        if let Some(signal) = self.signals.received() {
            return Ok(SupervisedOutput {
                result: SupervisedResult {
                    status: None,
                    signal: None,
                    timed_out: false,
                    interrupted_signal: Some(signal),
                },
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        let mut command = spec.command()?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| SupervisionError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        if let Err(error) = self.job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            self.job.terminate();
            let _ = child.wait();
            return Err(error);
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_reader = thread::spawn(move || read_pipe(stdout));
        let stderr_reader = thread::spawn(move || read_pipe(stderr));
        let result = self.monitor(&mut child, options, writer);
        let stdout = captured_bytes(stdout_reader, "stdout")?;
        let stderr = captured_bytes(stderr_reader, "stderr")?;
        Ok(SupervisedOutput {
            result: result?,
            stdout,
            stderr,
        })
    }

    fn monitor(
        &self,
        child: &mut Child,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        let started = Instant::now();
        let mut next_diagnostic = started + options.diagnostic_interval;
        let timeout_at = options.timeout.map(|timeout| started + timeout);
        let mut termination: Option<Instant> = None;
        let mut timed_out = false;
        let mut interrupted_signal = None;
        let mut escalated = false;

        loop {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    self.job.terminate();
                    let _ = child.wait();
                    return Err(SupervisionError::Wait(error));
                }
            };
            if let Some(status) = status {
                let (status, signal) = exit_parts(status);
                return Ok(SupervisedResult {
                    status,
                    signal,
                    timed_out,
                    interrupted_signal,
                });
            }
            let now = Instant::now();
            if termination.is_none()
                && let Some(signal) = self.signals.received()
            {
                interrupted_signal = Some(signal);
                forward_windows_control(child);
                termination = Some(now);
            }
            if termination.is_none() && timeout_at.is_some_and(|deadline| now >= deadline) {
                timed_out = true;
                let _ = write_line(
                    writer,
                    &format!(
                        "[supercov] command exceeded SUPERCOV_COMMAND_TIMEOUT_MS={}; terminating process group",
                        options.timeout.expect("timeout deadline").as_millis()
                    ),
                );
                forward_windows_control(child);
                termination = Some(now);
                write_diagnostic(child, started, writer);
            }
            if now >= next_diagnostic && !timed_out {
                write_diagnostic(child, started, writer);
                while next_diagnostic <= now {
                    next_diagnostic += options.diagnostic_interval;
                }
            }
            if !escalated
                && termination.is_some_and(|terminated_at| {
                    now.duration_since(terminated_at) >= options.termination_grace
                })
            {
                self.job.terminate();
                escalated = true;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub struct ProcessSupervisor;

#[cfg(not(any(unix, windows)))]
impl ProcessSupervisor {
    pub fn new() -> Result<Self, SupervisionError> {
        Err(SupervisionError::UnsupportedPlatform(
            "this target has no process-tree containment implementation",
        ))
    }

    pub fn new_crash_safe(_watchdog_program: &Path) -> Result<Self, SupervisionError> {
        Self::new()
    }

    pub fn supervise(
        &self,
        _spec: &CommandSpec,
        _options: SupervisionOptions,
        _writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        Err(SupervisionError::UnsupportedPlatform(
            "this target has no process-tree containment implementation",
        ))
    }

    pub fn supervise_captured(
        &self,
        _spec: &CommandSpec,
        _options: SupervisionOptions,
        _writer: &mut dyn Write,
    ) -> Result<SupervisedOutput, SupervisionError> {
        Err(SupervisionError::UnsupportedPlatform(
            "this target has no process-tree containment implementation",
        ))
    }
}

pub fn supervise_command(
    spec: &CommandSpec,
    options: SupervisionOptions,
    writer: &mut dyn Write,
) -> Result<SupervisedResult, SupervisionError> {
    ProcessSupervisor::new()?.supervise(spec, options, writer)
}

pub fn supervise_captured_command(
    spec: &CommandSpec,
    options: SupervisionOptions,
    writer: &mut dyn Write,
) -> Result<SupervisedOutput, SupervisionError> {
    ProcessSupervisor::new()?.supervise_captured(spec, options, writer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Somewhere a relay can write that the test can read afterwards.
    #[derive(Clone, Default)]
    struct Kept(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for Kept {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn what_a_command_prints_names_the_project() {
        // Go names a file in a compile error from where it runs, Maven and
        // Gradle end a failed run with the address of a report, and each
        // named Supercov's copy.
        let names = ConsoleNames {
            places: vec![(
                "/home/me/shop/.supercov/workspaces/workspace/shop".into(),
                "/home/me/shop".into(),
            )],
            up_to_the_project: Some("../../../../".into()),
        };
        let printed = "# example.com/shop [example.com/shop.test]\n../../../../crypto.go:4:15: undefined: undefinedThing\n../../../../pkg/inner/inner.go:4:9: cannot use s\n\tat ../../../../../elsewhere.go:1 and a/../../../../b.go\n[ERROR] Please refer to /home/me/shop/.supercov/workspaces/workspace/shop/target/surefire-reports for the individual test results.\nlast line, with no end";
        let kept = Kept::default();
        let ended = relay(
            io::Cursor::new(printed.as_bytes().to_vec()),
            kept.clone(),
            names,
        );
        ended
            .recv_timeout(Duration::from_secs(10))
            .expect("the relay ends with its input");
        assert_eq!(
            String::from_utf8(kept.0.lock().unwrap().clone()).unwrap(),
            "# example.com/shop [example.com/shop.test]\n./crypto.go:4:15: undefined: undefinedThing\npkg/inner/inner.go:4:9: cannot use s\n\tat ../../../../../elsewhere.go:1 and a/../../../../b.go\n[ERROR] Please refer to /home/me/shop/target/surefire-reports for the individual test results.\nlast line, with no end"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_command_on_a_terminal_still_writes_to_one_and_names_the_project() {
        use std::os::fd::{AsRawFd, FromRawFd};
        // The user's terminal: what is written to `users` shows at `screen`.
        let (mut screen, mut users) = (-1, -1);
        // SAFETY: the two descriptors are owned by nothing else.
        let (mut screen, users) = unsafe {
            assert_eq!(
                libc::openpty(
                    &mut screen,
                    &mut users,
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<libc::termios>(),
                    std::ptr::null_mut::<libc::winsize>(),
                ),
                0
            );
            let size = libc::winsize {
                ws_row: 40,
                ws_col: 132,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(users, libc::TIOCSWINSZ, &size);
            (
                std::fs::File::from_raw_fd(screen),
                std::fs::File::from_raw_fd(users),
            )
        };
        let (terminal, theirs) = Terminal::like(users).expect("a terminal like the user's");
        let names = ConsoleNames {
            places: vec![(
                "/home/me/shop/.supercov/workspaces/workspace/shop".into(),
                "/home/me/shop".into(),
            )],
            up_to_the_project: None,
        };
        // A line whose name comes in two writes, the terminal's width as the
        // command reads it, and a question that has no end of line.
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "test -t 1 && printf 'a terminal %s wide\\n' \"$(stty size <&2 | cut -d' ' -f2)\"; printf 'see /home/me/shop/.supercov/work'; printf 'spaces/workspace/shop/target/report\\n'; printf 'Install? '",
            ])
            .stdin(Stdio::null())
            .stdout(theirs.try_clone().unwrap())
            .stderr(theirs);
        let mut child = command.spawn().unwrap();
        drop(command);
        let ended = relay_terminal(terminal, names);
        assert!(child.wait().unwrap().success());
        ended
            .recv_timeout(Duration::from_secs(10))
            .expect("the relay ends when the command has gone");
        // SAFETY: a flag on a descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(screen.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(screen.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut shown = Vec::new();
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = screen.read(&mut buffer) {
            if read == 0 {
                break;
            }
            shown.extend_from_slice(&buffer[..read]);
        }
        // The user's terminal ends each line itself, once.
        assert_eq!(
            String::from_utf8(shown).unwrap(),
            "a terminal 132 wide\r\nsee /home/me/shop/target/report\r\nInstall? "
        );
    }

    fn scratch_directory(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn resolves_programs_the_way_the_windows_shell_does() {
        let root = scratch_directory("program-resolution");
        std::fs::create_dir(root.join("sub")).unwrap();
        // `tool.CMD` is the same file as `tool.cmd` where names are
        // case-insensitive and a second file where they are not; either way
        // the default extension list finds it under its own spelling.
        for name in ["tool.cmd", "tool.CMD", "named.exe", "plain", "sub/tool.cmd"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        let path = Some(std::env::join_paths([root.join("elsewhere"), root.clone()]).unwrap());
        let pathext = Some(OsString::from(".exe;.cmd"));
        let resolve = |program: &str| {
            resolve_program_with(OsStr::new(program), &root, path.clone(), pathext.clone())
        };

        assert_eq!(resolve("tool"), root.join("tool.cmd").into_os_string());
        assert_eq!(resolve("named"), root.join("named.exe").into_os_string());
        assert_eq!(
            resolve("named.exe"),
            root.join("named.exe").into_os_string()
        );
        // An extensionless file is never chosen: cmd.exe cannot start it either.
        assert_eq!(resolve("plain"), OsString::from("plain"));
        assert_eq!(resolve("missing"), OsString::from("missing"));
        // A separator means relative to the working directory, not PATH.
        assert_eq!(
            resolve("sub/tool"),
            root.join("sub/tool.cmd").into_os_string()
        );
        assert_eq!(
            resolve("./sub/tool"),
            root.join("./sub/tool.cmd").into_os_string()
        );
        let absolute = root.join("named.exe");
        assert_eq!(
            resolve_program_with(
                absolute.as_os_str(),
                Path::new("/nowhere"),
                None,
                pathext.clone()
            ),
            absolute.clone().into_os_string()
        );
        // No PATHEXT at all means the Windows default list.
        assert_eq!(
            resolve_program_with(OsStr::new("tool"), &root, path.clone(), None),
            root.join("tool.CMD").into_os_string()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn starts_a_batch_shim_found_through_pathext() {
        let root = scratch_directory("batch-shim");
        std::fs::write(root.join("shim.cmd"), "@echo off\r\nexit /b 3\r\n").unwrap();
        let mut environment = vec![
            (OsString::from("Path"), root.clone().into_os_string()),
            (
                OsString::from("PATHEXT"),
                OsString::from(".COM;.EXE;.BAT;.CMD"),
            ),
        ];
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            environment.push((OsString::from("SystemRoot"), system_root));
        }
        let spec = CommandSpec {
            program: "shim".into(),
            arguments: vec!["ignored".into()],
            cwd: std::env::current_dir().unwrap(),
            environment: Some(environment),
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let result =
            supervise_command(&spec, SupervisionOptions::default(), &mut diagnostics).unwrap();
        assert_eq!(result.exit_code(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_only_positive_integer_milliseconds() {
        assert_eq!(positive_milliseconds(None, "VALUE").unwrap(), None);
        assert_eq!(
            positive_milliseconds(Some("50"), "VALUE").unwrap(),
            Some(Duration::from_millis(50))
        );
        for value in ["0", "-1", "1.5", "NaN", " 1"] {
            assert!(positive_milliseconds(Some(value), "VALUE").is_err());
        }
    }

    #[test]
    fn diagnostic_format_is_sanitized_and_reference_compatible() {
        let output = format_process_diagnostic(
            20,
            Duration::from_millis(61_000),
            &[ProcessSnapshot {
                pid: 20,
                parent_pid: 10,
                executable: "node".into(),
                state: Some("S".into()),
                cpu_tenths: Some(13),
            }],
        );
        assert_eq!(
            output,
            "[supercov] command still running after 1m01s\n  pid=20 ppid=10 exe=node state=S cpu=1.3s"
        );
        assert!(!output.contains("argv"));
    }

    #[cfg(unix)]
    #[test]
    fn returns_the_child_status_without_a_default_timeout() {
        let root = std::env::current_dir().unwrap();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "exit 7".into()],
            cwd: root,
            environment: None,
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let result =
            supervise_command(&spec, SupervisionOptions::default(), &mut diagnostics).unwrap();
        assert_eq!(result.exit_code(), 7);
        assert!(!result.timed_out);
        assert!(diagnostics.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn captures_stdout_and_stderr_separately_without_losing_status() {
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf stdout-value; printf stderr-value >&2; exit 9".into(),
            ],
            cwd: std::env::current_dir().unwrap(),
            environment: None,
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let output =
            supervise_captured_command(&spec, SupervisionOptions::default(), &mut diagnostics)
                .unwrap();
        assert_eq!(output.result.exit_code(), 9);
        assert_eq!(output.stdout, b"stdout-value");
        assert_eq!(output.stderr, b"stderr-value");
        assert!(diagnostics.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn returns_the_windows_child_status_without_a_default_timeout() {
        let spec = CommandSpec {
            program: "cmd.exe".into(),
            arguments: vec!["/D".into(), "/S".into(), "/C".into(), "exit /b 7".into()],
            cwd: std::env::current_dir().unwrap(),
            environment: None,
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let result =
            supervise_command(&spec, SupervisionOptions::default(), &mut diagnostics).unwrap();
        assert_eq!(result.exit_code(), 7);
        assert!(!result.timed_out);
        assert!(diagnostics.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_timeout_reports_and_returns_124() {
        let root = std::env::current_dir().unwrap();
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "while :; do sleep 1; done".into()],
            cwd: root,
            environment: None,
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let result = supervise_command(
            &spec,
            SupervisionOptions {
                diagnostic_interval: Duration::from_millis(20),
                timeout: Some(Duration::from_millis(70)),
                termination_grace: Duration::from_millis(50),
            },
            &mut diagnostics,
        )
        .unwrap();
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert_eq!(result.exit_code(), COMMAND_TIMEOUT_EXIT_CODE);
        assert!(result.timed_out);
        assert!(diagnostics.contains("command still running after"));
        assert!(diagnostics.contains("SUPERCOV_COMMAND_TIMEOUT_MS=70"));
    }

    #[cfg(windows)]
    #[test]
    fn timeout_terminates_the_complete_windows_job() {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-windows-job-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        struct RemoveOnDrop(PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveOnDrop(root.clone());
        let ready = root.join("descendant-ready");
        let marker = root.join("descendant-survived");
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.extend([
            ("SUPERCOV_WINDOWS_PARENT_HELPER".into(), "1".into()),
            ("SUPERCOV_WINDOWS_READY".into(), ready.as_os_str().into()),
            ("SUPERCOV_WINDOWS_MARKER".into(), marker.as_os_str().into()),
        ]);
        let spec = CommandSpec {
            program: std::env::current_exe().unwrap().into_os_string(),
            arguments: vec![
                "--ignored".into(),
                "windows_timeout_parent_helper".into(),
                "--nocapture".into(),
            ],
            cwd: root,
            environment: Some(environment),
            captured_output: None,
            console: None,
        };
        let mut diagnostics = Vec::new();
        let result = supervise_command(
            &spec,
            SupervisionOptions {
                diagnostic_interval: Duration::from_secs(60),
                timeout: Some(Duration::from_millis(750)),
                termination_grace: Duration::from_millis(50),
            },
            &mut diagnostics,
        )
        .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.exit_code(), COMMAND_TIMEOUT_EXIT_CODE);
        assert!(
            ready.exists(),
            "the helper did not prove that its descendant started before timeout"
        );
        thread::sleep(Duration::from_millis(1_700));
        assert!(
            !marker.exists(),
            "a descendant escaped the Windows Job Object after timeout"
        );
        assert!(
            String::from_utf8(diagnostics)
                .unwrap()
                .contains("terminating process group")
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess helper for timeout_terminates_the_complete_windows_job"]
    fn windows_timeout_parent_helper() {
        use std::fs;

        if std::env::var_os("SUPERCOV_WINDOWS_PARENT_HELPER").is_none() {
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "windows_timeout_marker_helper", "--nocapture"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(
            std::env::var_os("SUPERCOV_WINDOWS_READY").unwrap(),
            child.id().to_string(),
        )
        .unwrap();
        child.wait().unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess helper for timeout_terminates_the_complete_windows_job"]
    fn windows_timeout_marker_helper() {
        use std::fs;

        if std::env::var_os("SUPERCOV_WINDOWS_PARENT_HELPER").is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(1_500));
        fs::write(
            std::env::var_os("SUPERCOV_WINDOWS_MARKER").unwrap(),
            b"escaped",
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_write_failures_never_change_the_child_result() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "closed diagnostic stream",
                ))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let spec = CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "sleep 0.05; exit 0".into()],
            cwd: std::env::current_dir().unwrap(),
            environment: None,
            captured_output: None,
            console: None,
        };
        let result = supervise_command(
            &spec,
            SupervisionOptions {
                diagnostic_interval: Duration::from_millis(10),
                timeout: None,
                termination_grace: Duration::from_millis(50),
            },
            &mut BrokenWriter,
        )
        .unwrap();
        assert_eq!(result.exit_code(), 0);
    }
}
