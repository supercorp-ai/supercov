//! Privacy-preserving child-process supervision for arbitrary test commands.

use std::{
    ffi::OsString,
    io::{self, Write},
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
}

impl CommandSpec {
    pub fn command(&self) -> Result<Command, SupervisionError> {
        if self.program.is_empty() {
            return Err(SupervisionError::EmptyCommand);
        }
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .current_dir(&self.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(environment) = &self.environment {
            command.env_clear().envs(environment.iter().cloned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Ok(command)
    }
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
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};

    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing().with_cpu()),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessSnapshot {
            pid: pid.as_u32(),
            parent_pid: process.parent().map_or(0, sysinfo::Pid::as_u32),
            state: Some(process_status(process.status()).into()),
            cpu_tenths: Some(process.accumulated_cpu_time() / 100),
            executable: Path::new(process.name())
                .file_name()
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
        match RECEIVED_SIGNAL.swap(0, Ordering::SeqCst) {
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

fn write_diagnostic(child: &Child, started: Instant, writer: &mut dyn Write) {
    let tree = descendant_process_tree(child.id());
    let _ = writeln!(
        writer,
        "{}",
        format_process_diagnostic(child.id(), started.elapsed(), &tree)
    )
    .and_then(|_| writer.flush());
}

#[cfg(unix)]
pub struct ProcessSupervisor {
    signals: SignalFlags,
}

#[cfg(unix)]
impl ProcessSupervisor {
    pub fn new() -> Result<Self, SupervisionError> {
        Ok(Self {
            signals: SignalFlags::install()?,
        })
    }

    pub fn supervise(
        &self,
        spec: &CommandSpec,
        options: SupervisionOptions,
        writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
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
        if let Some(signal) = self.signals.received() {
            return Ok(SupervisedResult {
                status: None,
                signal: Some(signal.raw()),
                timed_out: false,
                interrupted_signal: Some(signal),
            });
        }
        let mut command = spec.command()?;
        let mut child = command.spawn().map_err(|source| SupervisionError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        let started = Instant::now();
        let mut next_diagnostic = started + options.diagnostic_interval;
        let timeout_at = options.timeout.map(|timeout| started + timeout);
        let mut termination: Option<(Instant, Option<ForwardedSignal>)> = None;
        let mut timed_out = false;
        let mut interrupted_signal = None;
        let mut escalated = false;

        loop {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    signal_process_group(&mut child, libc::SIGKILL);
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
                signal_process_group(&mut child, signal.raw());
                termination = Some((now, Some(signal)));
            }
            if termination.is_none() && timeout_at.is_some_and(|deadline| now >= deadline) {
                timed_out = true;
                let _ = writeln!(
                writer,
                "[supercov] command exceeded SUPERCOV_COMMAND_TIMEOUT_MS={}; terminating process group",
                options.timeout.expect("timeout deadline").as_millis()
            )
            .and_then(|_| writer.flush());
                signal_process_group(&mut child, libc::SIGTERM);
                termination = Some((now, None));
                write_diagnostic(&child, started, writer);
            }
            if now >= next_diagnostic && !timed_out {
                write_diagnostic(&child, started, writer);
                while next_diagnostic <= now {
                    next_diagnostic += options.diagnostic_interval;
                }
            }
            if !escalated
                && termination.is_some_and(|(terminated_at, _)| {
                    now.duration_since(terminated_at) >= options.termination_grace
                })
            {
                signal_process_group(&mut child, libc::SIGKILL);
                escalated = true;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(not(unix))]
pub struct ProcessSupervisor;

#[cfg(not(unix))]
impl ProcessSupervisor {
    pub fn new() -> Result<Self, SupervisionError> {
        Err(SupervisionError::UnsupportedPlatform(
            "Windows Job Objects are required before enabling the Rust supervisor",
        ))
    }

    pub fn supervise(
        &self,
        _spec: &CommandSpec,
        _options: SupervisionOptions,
        _writer: &mut dyn Write,
    ) -> Result<SupervisedResult, SupervisionError> {
        Err(SupervisionError::UnsupportedPlatform(
            "Windows Job Objects are required before enabling the Rust supervisor",
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

#[cfg(test)]
mod tests {
    use super::*;

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
