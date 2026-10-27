//! Faults a test injects, so the code that handles a system call failing runs
//! under test. `SUPERCOV_TEST_FAULT` names call sites, comma-separated: `site`
//! fails every call there, `site=N` only the first N, `site@N` only the Nth,
//! `site#N` only the first call from the Nth place in the code that calls it
//! (where the site knows its caller, as [`crate::fault_fs`] does), and
//! `site:E` (or `site:E=N`, `site:E@N`) fails it with errno `E` instead of
//! its usual error. An entry written `__command/site...` applies only in a
//! process started as `supercov __command` -- a doctest's or nextest's runner
//! -- and one written `env:NAME/site...` only in a process the variable
//! `NAME` is set for -- a merged doctest's child -- so a test can fail its
//! Nth place there and not its parent's. A failing
//! site does not make its call; it returns what the call returns when it
//! fails, and says so on standard error (`test fault at <site>`), so a test
//! knows the site was reached. Only debug builds read the variable: a release
//! build has no such switch. The engine's own tests set faults for one thread with [`during`].
//! Every part of it is compiled into every debug build alike: a part only
//! the engine's test build had would make that build's coverage count lines
//! the others' do not, and runs of the two could not be merged.

use std::io;

#[cfg(any(test, debug_assertions))]
struct Fault {
    site: String,
    errno: Option<i32>,
    /// Calls left to pass before the first that fails.
    skip: std::sync::atomic::AtomicUsize,
    /// Calls left to fail; `None` for every call.
    remaining: Option<std::sync::atomic::AtomicUsize>,
    /// For `site#N`: N, and the places in the code that called the site so
    /// far, in the order they first did.
    place: Option<(usize, std::sync::Mutex<Vec<Place>>)>,
}

/// Where in the code a call was made, and which call it was: a helper marked
/// `#[track_caller]` hands its caller's place to the calls it makes, and two
/// different calls from there are two places.
pub type Place = (&'static std::panic::Location<'static>, &'static str);

#[cfg(any(test, debug_assertions))]
static FAULTS: std::sync::LazyLock<Vec<Fault>> =
    std::sync::LazyLock::new(|| parse(&std::env::var("SUPERCOV_TEST_FAULT").unwrap_or_default()));

/// The internal command this process was started as (`__doctest-runner`),
/// or nothing for any other.
#[cfg(any(test, debug_assertions))]
fn process_role() -> String {
    std::env::args_os()
        .nth(1)
        .and_then(|first| first.into_string().ok())
        .filter(|first| first.starts_with("__"))
        .unwrap_or_default()
}

#[cfg(any(test, debug_assertions))]
fn parse(text: &str) -> Vec<Fault> {
    let role = process_role();
    text.split(',')
        .filter(|entry| !entry.is_empty())
        // An entry for another process's role is none of this one's.
        .filter_map(|entry| match entry.split_once('/') {
            Some((named, rest)) => match named.strip_prefix("env:") {
                Some(variable) => std::env::var_os(variable).is_some(),
                None => named == role,
            }
            .then_some(rest),
            None => Some(entry),
        })
        .map(|entry| {
            let (site, count) = entry.split_once('=').unwrap_or((entry, ""));
            let (site, place) = site.split_once('#').unwrap_or((site, ""));
            let (site, nth) = site.split_once('@').unwrap_or((site, ""));
            let (site, errno) = site.split_once(':').unwrap_or((site, ""));
            let nth = nth.parse::<usize>().ok();
            Fault {
                site: site.to_owned(),
                errno: errno.parse().ok(),
                skip: nth.map_or(0, |nth| nth.saturating_sub(1)).into(),
                remaining: nth
                    .map(|_| 1)
                    .or_else(|| count.parse().ok())
                    .map(std::sync::atomic::AtomicUsize::new),
                place: place
                    .parse::<usize>()
                    .ok()
                    .map(|nth| (nth, std::sync::Mutex::new(Vec::new()))),
            }
        })
        .collect()
}

/// Read the switch now. A child forked to exec a command may not allocate,
/// so the sites it reaches are read before the fork.
pub fn prepare() {
    #[cfg(any(test, debug_assertions))]
    std::sync::LazyLock::force(&FAULTS);
}

/// Whether the call at `site` fails this time.
pub fn injected(site: &str) -> bool {
    failing(site).is_some()
}

/// [`injected`], for the call `operation` made where the caller of a
/// `#[track_caller]` function is: `site#N` counts the places it is called
/// from.
#[track_caller]
pub fn injected_here(site: &str, operation: &'static str) -> bool {
    failing_at(site, Some((std::panic::Location::caller(), operation))).is_some()
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static DURING: std::cell::RefCell<Vec<Fault>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `body` with the faults `faults` names (as `SUPERCOV_TEST_FAULT` does)
/// on this thread.
#[cfg(any(test, debug_assertions))]
pub fn during<R>(faults: &str, body: impl FnOnce() -> R) -> R {
    let earlier = DURING.with(|during| during.replace(parse(faults)));
    let result = body();
    DURING.with(|during| during.replace(earlier));
    result
}

/// When the call at `site` fails this time, the errno the test chose for
/// it, if any.
fn failing(site: &str) -> Option<Option<i32>> {
    failing_at(site, None)
}

/// [`failing`], for a call made at `place`.
fn failing_at(site: &str, place: Option<Place>) -> Option<Option<i32>> {
    #[cfg(any(test, debug_assertions))]
    {
        // This thread's faults first, the variable's where they name nothing.
        DURING
            .with(|during| take(&during.borrow(), site, place))
            .or_else(|| take(&FAULTS, site, place))
            .flatten()
            .inspect(|_| announce(site))
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        let _ = (site, place);
        None
    }
}

/// Say that the call at `site` failed by request. Written to the descriptor
/// itself: a child forked to exec a command may not allocate or take the
/// lock `eprintln!` takes.
#[cfg(any(test, debug_assertions))]
fn announce(site: &str) {
    #[cfg(unix)]
    for part in [
        b"[supercov] test fault at ".as_slice(),
        site.as_bytes(),
        b"\n",
    ] {
        // SAFETY: a write of a live buffer to standard error.
        unsafe { libc::write(2, part.as_ptr().cast(), part.len()) };
    }
    #[cfg(not(unix))]
    eprintln!("[supercov] test fault at {site}");
}

/// Of the faults `faults`, whether the one at `site` fails this time, and
/// with which errno: `None` when no fault names the site.
#[cfg(any(test, debug_assertions))]
fn take(faults: &[Fault], site: &str, place: Option<Place>) -> Option<Option<Option<i32>>> {
    use std::sync::atomic::Ordering;
    let count_down = |left: &std::sync::atomic::AtomicUsize| {
        left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
            left.checked_sub(1)
        })
        .is_ok()
    };
    // Every entry naming the site counts its own calls, so `fs@3,fs@7`
    // fails the third and the seventh.
    let mut named = false;
    let mut failed = None;
    for fault in faults.iter().filter(|fault| fault.site == site) {
        named = true;
        let fails = match (&fault.place, place) {
            // The first call from a place not seen before, when it is the Nth.
            (Some((nth, seen)), Some(place)) => {
                let mut seen = seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                !seen.contains(&place) && {
                    seen.push(place);
                    seen.len() == *nth
                }
            }
            // A caller that cannot say where it is is never the Nth place.
            (Some(_), None) => false,
            (None, _) => {
                !count_down(&fault.skip) && fault.remaining.as_ref().is_none_or(count_down)
            }
        };
        if fails {
            failed.get_or_insert(fault.errno);
        }
    }
    named.then_some(failed)
}

/// `call`'s value, or `failed`'s when the call at `site` fails.
pub fn or<T>(site: &str, failed: impl FnOnce() -> T, call: impl FnOnce() -> T) -> T {
    if injected(site) { failed() } else { call() }
}

/// `call`'s result, or an error when the call at `site` fails: of `kind`, or
/// the system's error for the errno the test chose.
pub fn io<T>(
    site: &str,
    kind: io::ErrorKind,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    match failing(site) {
        Some(Some(errno)) => Err(io::Error::from_raw_os_error(errno)),
        Some(None) => Err(io::Error::new(kind, format!("injected fault at {site}"))),
        None => call(),
    }
}

/// Standard error, held for a run's diagnostics: each write is a call at the
/// site `diagnostics`, so `diagnostics@N` fails the run's Nth write there.
pub fn diagnostics() -> Writer<io::StderrLock<'static>> {
    Writer {
        site: "diagnostics",
        inner: io::stderr().lock(),
    }
}

/// A writer whose writes are calls at `site`.
pub struct Writer<W> {
    site: &'static str,
    inner: W,
}

impl<W: io::Write> io::Write for Writer<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io(self.site, io::ErrorKind::BrokenPipe, || {
            self.inner.write(bytes)
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// [`io`], for the call `operation` made where the caller of a
/// `#[track_caller]` function is: `site#N` counts the places it is called
/// from.
#[track_caller]
pub fn io_here<T>(
    site: &str,
    operation: &'static str,
    kind: io::ErrorKind,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    io_at(
        site,
        kind,
        (std::panic::Location::caller(), operation),
        call,
    )
}

/// [`io_here`], for a call made at `place`.
pub fn io_at<T>(
    site: &str,
    kind: io::ErrorKind,
    place: Place,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    match failing_at(site, Some(place)) {
        Some(Some(errno)) => Err(io::Error::from_raw_os_error(errno)),
        Some(None) => Err(io::Error::new(kind, format!("injected fault at {site}"))),
        None => call(),
    }
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
