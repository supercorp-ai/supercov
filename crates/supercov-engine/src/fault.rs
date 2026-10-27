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
//! fails, and says so on standard error (`test fault at <site>`), and as a
//! line of the file `SUPERCOV_TEST_FAULT_LOG` names, where it names one: a
//! runner's standard error is its parent's to read. So a test knows the site
//! was reached. Only debug builds read the variable: a release
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
    /// Whether a call failed by it.
    fired: std::sync::atomic::AtomicBool,
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

/// The file a failing site is written to as well, as a C string: a forked
/// child writes it without allocating.
#[cfg(all(unix, any(test, debug_assertions)))]
static LOG: std::sync::LazyLock<Option<std::ffi::CString>> = std::sync::LazyLock::new(|| {
    use std::os::unix::ffi::OsStringExt;
    std::env::var_os("SUPERCOV_TEST_FAULT_LOG")
        .and_then(|path| std::ffi::CString::new(path.into_vec()).ok())
});

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
                fired: false.into(),
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
    #[cfg(all(unix, any(test, debug_assertions)))]
    std::sync::LazyLock::force(&LOG);
}

/// Whether the call at `site` fails this time.
pub fn injected(site: &str) -> bool {
    failing(site).is_some()
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static DURING: std::cell::RefCell<Vec<Fault>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `body` with the faults `faults` names (as `SUPERCOV_TEST_FAULT` does)
/// on this thread.
#[cfg(any(test, debug_assertions))]
pub fn during<R>(faults: &str, body: impl FnOnce() -> R) -> R {
    during_reaching(faults, body).0
}

/// [`during`]'s result, and whether any of the faults failed a call: a test
/// failing each call of a site in turn (`site@1`, `site@2`, ...) stops at the
/// first run that no longer reaches its fault.
#[cfg(any(test, debug_assertions))]
pub fn during_reaching<R>(faults: &str, body: impl FnOnce() -> R) -> (R, bool) {
    let earlier = DURING.with(|during| during.replace(parse(faults)));
    let result = body();
    let set = DURING.with(|during| during.replace(earlier));
    let reached = set
        .iter()
        .any(|fault| fault.fired.load(std::sync::atomic::Ordering::SeqCst));
    (result, reached)
}

/// Run `scenario` once for each call it makes at `site`, with that one
/// failing (`site@1`, `site@2`, ...): every run must end, with an error or
/// without, and never panic, and the run that reaches no fault must succeed.
/// The number of calls, so a test knows the sweep reached them.
#[cfg(any(test, debug_assertions))]
pub fn fail_each_call<Fixture, E: std::fmt::Debug>(
    site: &str,
    setup: impl Fn() -> Fixture,
    scenario: impl Fn(&Fixture) -> Result<(), E>,
    cleanup: impl Fn(Fixture),
) -> usize {
    let mut nth = 0;
    loop {
        nth += 1;
        let fixture = setup();
        let (result, reached) = during_reaching(&format!("{site}@{nth}"), || scenario(&fixture));
        cleanup(fixture);
        if !reached {
            result.expect("the run no fault reached succeeds");
            return nth - 1;
        }
    }
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
    #[cfg(unix)]
    if let Some(path) = LOG.as_ref() {
        // SAFETY: an open of a live C string, writes of live buffers to the
        // descriptor it gave, and its close. A log that could not be opened
        // is descriptor -1, which the writes and the close refuse.
        unsafe {
            let log = libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT,
                0o644,
            );
            for part in [site.as_bytes(), b"\n"] {
                libc::write(log, part.as_ptr().cast(), part.len());
            }
            libc::close(log);
        }
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
    faults.iter().find(|fault| fault.site == site).map(|fault| {
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
            fault.fired.store(true, Ordering::SeqCst);
        }
        fails.then_some(fault.errno)
    })
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
