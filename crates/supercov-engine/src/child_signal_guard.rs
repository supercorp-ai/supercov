//! Take the test processes down with the runner.
//!
//! The owned Rust runner starts every libtest case in a process of its own and
//! waits for it. A signal that reaches the runner alone -- a supervisor's
//! SIGTERM, a `kill <pid>` from a script -- ended the runner and left the test
//! process running: on 2026-09-07 an orphaned tokio `test_tuning` spun on two
//! cores for an hour and starved every run that followed. Ctrl-C in a terminal
//! never showed this, because the shell signals the whole foreground process
//! group and the child dies with its parent.
//!
//! While a guard is installed, SIGHUP, SIGINT and SIGTERM first send SIGTERM
//! to every registered child, then let the signal take its default action, so
//! the runner exits exactly as it did before. The handler touches nothing but
//! a fixed table of atomics and `kill(2)`, both of which are safe inside a
//! signal handler.
//!
//! Windows delivers no signal a process can catch, but the kernel closes
//! every handle a dying process held: while a guard is installed, each
//! registered child is placed in a Job Object that kills its members when its
//! handle closes, so the test processes die with the runner however it ends.

use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

/// How many children can be registered at once. The runner runs at most one
/// child per worker thread, and machines with more cores than this are rare;
/// a child that finds no free slot is simply not tracked.
const SLOTS: usize = 512;
static CHILDREN: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];
static GUARDS: AtomicUsize = AtomicUsize::new(0);

/// Send `signal` to every registered child. Safe to call from a signal
/// handler: it only loads atomics and calls `kill(2)`.
pub fn signal_registered_children(signal: i32) {
    for slot in &CHILDREN {
        let pid = slot.load(Ordering::SeqCst);
        if pid > 0 {
            #[cfg(unix)]
            // SAFETY: `kill` with a positive pid signals that process only.
            unsafe {
                libc::kill(pid, signal);
            }
            #[cfg(not(unix))]
            let _ = signal;
        }
    }
}

/// A child's registration; dropping it removes the child from the table.
pub struct RegisteredChild {
    slot: Option<usize>,
}

impl Drop for RegisteredChild {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            CHILDREN[slot].store(0, Ordering::SeqCst);
        }
    }
}

/// Register a running child so a signal to the runner reaches it.
pub fn register(child: &Child) -> RegisteredChild {
    let Ok(pid) = i32::try_from(child.id()) else {
        return RegisteredChild { slot: None };
    };
    #[cfg(windows)]
    windows_job::assign(child);
    for (index, slot) in CHILDREN.iter().enumerate() {
        if slot
            .compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return RegisteredChild { slot: Some(index) };
        }
    }
    RegisteredChild { slot: None }
}

#[cfg(windows)]
mod windows_job {
    use std::process::Child;
    use std::sync::Mutex;

    use crate::process_supervision::JobHandle;

    /// One job for the guard's lifetime; children assigned to it die when the
    /// runner's handle closes, which the kernel does however the runner ends.
    static JOB: Mutex<Option<JobHandle>> = Mutex::new(None);

    fn job() -> std::sync::MutexGuard<'static, Option<JobHandle>> {
        JOB.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn create() {
        // Best effort: a runner that cannot create a job runs as it did.
        if let Ok(handle) = JobHandle::new() {
            *job() = Some(handle);
        }
    }

    pub fn release() {
        // Closing the handle kills whatever is still assigned; the runner has
        // waited for every child by the time the outermost guard drops.
        *job() = None;
    }

    pub fn assign(child: &Child) {
        // A child already inside a job that forbids nesting stays outside
        // ours; the runner behaves as it did before the guard existed.
        if let Some(handle) = job().as_ref() {
            let _ = handle.assign(child);
        }
    }
}

/// `Command::output()`, with the child registered while it runs: stdin
/// closed, stdout and stderr captured, exactly as `output()` does.
pub fn output(command: &mut Command) -> io::Result<Output> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let _registered = register(&child);
    child.wait_with_output()
}

/// `output`, also passing what the child writes on to this process's own
/// streams as it arrives: a test asked to run without capture shows its
/// output while it runs, and the run still reads all of it. A line of the
/// child's standard output that `keep` refuses is recorded and not shown.
pub fn output_streamed(command: &mut Command, keep: fn(&[u8]) -> bool) -> io::Result<Output> {
    use std::io::{Read, Write};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let _registered = register(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdout, stderr) = std::thread::scope(|scope| {
        let out = scope.spawn(move || {
            let mut recorded = Vec::new();
            let mut line = Vec::new();
            let mut chunk = [0_u8; 8192];
            let mut source = stdout.expect("piped above");
            while let Ok(read) = source.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                recorded.extend_from_slice(&chunk[..read]);
                for byte in &chunk[..read] {
                    line.push(*byte);
                    if *byte == b'\n' {
                        if keep(&line) {
                            let _ = io::stdout().lock().write_all(&line);
                        }
                        line.clear();
                    }
                }
            }
            if keep(&line) {
                let _ = io::stdout().lock().write_all(&line);
            }
            recorded
        });
        let err = scope.spawn(move || {
            let mut recorded = Vec::new();
            let mut chunk = [0_u8; 8192];
            let mut source = stderr.expect("piped above");
            while let Ok(read) = source.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                recorded.extend_from_slice(&chunk[..read]);
                let _ = io::stderr().lock().write_all(&chunk[..read]);
            }
            recorded
        });
        (
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default(),
        )
    });
    let status = child.wait()?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// `Command::status()`, with the child registered while it runs.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    let mut child = command.spawn()?;
    let _registered = register(&child);
    child.wait()
}

#[cfg(unix)]
extern "C" fn on_signal(signal: libc::c_int) {
    signal_registered_children(libc::SIGTERM);
    // The default action, delivered once this handler returns: the runner
    // dies of the signal exactly as it did before the guard existed.
    // SAFETY: both calls are async-signal-safe; `signal` came from the kernel.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Installs `on_signal` for each of `signals`, returning the dispositions it
/// replaced; when one cannot be installed, those already installed are put
/// back and the error is returned.
#[cfg(unix)]
fn install_handlers(signals: &[libc::c_int]) -> io::Result<Vec<(libc::c_int, libc::sighandler_t)>> {
    let mut previous = Vec::new();
    for &signal in signals {
        // SAFETY: installs a handler that is async-signal-safe
        // (see `on_signal`); the old disposition is kept to restore.
        let old = unsafe {
            libc::signal(
                signal,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
        if old == libc::SIG_ERR {
            let error = io::Error::last_os_error();
            for (installed, disposition) in previous.drain(..).rev() {
                // SAFETY: restores a disposition `signal` returned.
                unsafe {
                    libc::signal(installed, disposition);
                }
            }
            return Err(error);
        }
        previous.push((signal, old));
    }
    Ok(previous)
}

/// Installs the handlers for the guard's lifetime and restores the previous
/// dispositions when dropped. Guards nest: only the outermost installs.
pub struct ChildSignalGuard {
    #[cfg(unix)]
    previous: Vec<(libc::c_int, libc::sighandler_t)>,
}

impl ChildSignalGuard {
    pub fn install() -> io::Result<Self> {
        #[cfg(unix)]
        {
            let mut previous = Vec::new();
            if GUARDS.fetch_add(1, Ordering::SeqCst) == 0 {
                previous = install_handlers(&[libc::SIGHUP, libc::SIGINT, libc::SIGTERM])
                    .inspect_err(|_| {
                        GUARDS.fetch_sub(1, Ordering::SeqCst);
                    })?;
            }
            Ok(Self { previous })
        }
        #[cfg(not(unix))]
        {
            if GUARDS.fetch_add(1, Ordering::SeqCst) == 0 {
                #[cfg(windows)]
                windows_job::create();
            }
            Ok(Self {})
        }
    }
}

impl Drop for ChildSignalGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        for (signal, disposition) in self.previous.drain(..).rev() {
            // SAFETY: restores a disposition `signal` returned when installing.
            unsafe {
                libc::signal(signal, disposition);
            }
        }
        let outermost = GUARDS.fetch_sub(1, Ordering::SeqCst) == 1;
        #[cfg(windows)]
        if outermost {
            windows_job::release();
        }
        #[cfg(not(windows))]
        let _ = outermost;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// A streamed child is read whole, both streams, whatever is shown of
    /// its standard output on the way.
    #[test]
    fn a_streamed_child_is_recorded_whole() {
        let output = output_streamed(
            Command::new("sh").args([
                "-c",
                "echo kept; echo dropped; printf tail; echo err >&2; exit 3",
            ]),
            |line| !line.starts_with(b"dropped"),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"kept\ndropped\ntail");
        assert_eq!(output.stderr, b"err\n");
        assert!(output_streamed(&mut Command::new("/no/such/program"), |_| true).is_err());
    }

    fn sleeper() -> Child {
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn exits_within(child: &mut Child, limit: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The disposition `signal` has now.
    fn disposition(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: a null new action only reads the current one.
        unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut current), 0);
            current.sa_sigaction
        }
    }

    #[test]
    fn a_handler_that_cannot_be_installed_puts_back_those_that_were() {
        // SIGKILL cannot be caught; SIGUSR2, installed before it, must be
        // left as it was found.
        let before = disposition(libc::SIGUSR2);
        assert!(install_handlers(&[libc::SIGUSR2, libc::SIGKILL]).is_err());
        assert_eq!(disposition(libc::SIGUSR2), before);
    }

    #[test]
    fn only_the_outermost_guard_installs_and_restores_handlers() {
        let outer = ChildSignalGuard::install().unwrap();
        let inner = ChildSignalGuard::install().unwrap();
        assert!(
            inner.previous.is_empty(),
            "a nested guard replaced handlers"
        );
        drop(inner);
        drop(outer);
    }

    #[test]
    fn dropping_an_untracked_registration_frees_no_slot() {
        let mut child = sleeper();
        let registration = register(&child);
        let slot = registration.slot.unwrap();
        drop(RegisteredChild { slot: None });
        assert_eq!(
            CHILDREN[slot].load(Ordering::SeqCst),
            i32::try_from(child.id()).unwrap()
        );
        drop(registration);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_registered_child_receives_the_signal_and_a_released_one_does_not() {
        let _guard = ChildSignalGuard::install().unwrap();
        let mut tracked = sleeper();
        let mut released = sleeper();
        let registration = register(&tracked);
        drop(register(&released));

        // What the handler does, without dying of the signal ourselves.
        signal_registered_children(libc::SIGTERM);

        assert!(
            exits_within(&mut tracked, Duration::from_secs(5)),
            "the registered child was not signalled"
        );
        assert!(
            released.try_wait().unwrap().is_none(),
            "a child whose registration was dropped must not be signalled"
        );
        drop(registration);
        released.kill().unwrap();
        released.wait().unwrap();
    }
}
