//! Faults a test injects, so the code that handles a system call failing runs
//! under test. `SUPERCOV_TEST_FAULT` names call sites, comma-separated: `site`
//! fails every call there, `site=N` only the first N, and `site:E` (or
//! `site:E=N`) fails a libc call with errno `E` instead of its usual one. A
//! failing site does not make its call; it returns what the call returns when
//! it fails. Only debug builds read the variable: a release build has no such
//! switch.

use std::io;

#[cfg(debug_assertions)]
struct Fault {
    site: String,
    errno: Option<i32>,
    /// Calls left to fail; `None` for every call.
    remaining: Option<std::sync::atomic::AtomicUsize>,
}

#[cfg(debug_assertions)]
static FAULTS: std::sync::LazyLock<Vec<Fault>> =
    std::sync::LazyLock::new(|| parse(&std::env::var("SUPERCOV_TEST_FAULT").unwrap_or_default()));

#[cfg(debug_assertions)]
fn parse(text: &str) -> Vec<Fault> {
    text.split(',')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (site, count) = entry.split_once('=').unwrap_or((entry, ""));
            let (site, errno) = site.split_once(':').unwrap_or((site, ""));
            Fault {
                site: site.to_owned(),
                errno: errno.parse().ok(),
                remaining: count.parse().ok().map(std::sync::atomic::AtomicUsize::new),
            }
        })
        .collect()
}

/// Read the switch now. A child forked to exec a command may not allocate,
/// so the sites it reaches are read before the fork.
pub fn prepare() {
    #[cfg(debug_assertions)]
    std::sync::LazyLock::force(&FAULTS);
}

/// Whether the call at `site` fails this time.
pub fn injected(site: &str) -> bool {
    failing(site).is_some()
}

/// When the call at `site` fails this time, the errno the test chose for
/// it, if any.
fn failing(site: &str) -> Option<Option<i32>> {
    #[cfg(debug_assertions)]
    {
        use std::sync::atomic::Ordering;
        FAULTS
            .iter()
            .find(|fault| fault.site == site)
            .filter(|fault| {
                fault.remaining.as_ref().is_none_or(|remaining| {
                    remaining
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        })
                        .is_ok()
                })
            })
            .map(|fault| fault.errno)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = site;
        None
    }
}

/// `call`'s value, or `failed`'s when the call at `site` fails.
pub fn or<T>(site: &str, failed: impl FnOnce() -> T, call: impl FnOnce() -> T) -> T {
    if injected(site) { failed() } else { call() }
}

/// `call`'s result, or an error of `kind` when the call at `site` fails.
pub fn io<T>(
    site: &str,
    kind: io::ErrorKind,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    or(
        site,
        || Err(io::Error::new(kind, format!("injected fault at {site}"))),
        call,
    )
}

/// A libc call's return, or -1 with errno set when the call at `site`
/// fails, as the call itself reports failing: to `errno`, or to the one the
/// test chose. Safe in a forked child.
#[cfg(unix)]
pub fn libc<T: From<i8>>(site: &str, errno: i32, call: impl FnOnce() -> T) -> T {
    match failing(site) {
        Some(chosen) => {
            // SAFETY: the calling thread's errno is always writable.
            unsafe { *errno_location() = chosen.unwrap_or(errno) };
            T::from(-1)
        }
        None => call(),
    }
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut i32 {
    unsafe { libc::__errno_location() }
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn errno_location() -> *mut i32 {
    unsafe { libc::__error() }
}
