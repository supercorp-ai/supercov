//! Faults a test injects, so the code that handles a system call failing runs
//! under test. `SUPERCOV_TEST_FAULT` names call sites, comma-separated: `site`
//! fails every call there, `site=N` only the first N, `site@N` only the Nth,
//! `site#N` only the first call from the Nth place in the code that calls it
//! (where the site knows its caller, as [`crate::fault_fs`] does) and
//! `site#N@M` only its Mth -- the count of one place, which no walk over a
//! varying number of files moves -- and `site#NAME` (or `site#NAME@M`) the
//! call at the place a `// fault-site: NAME` comment on the line above it
//! names, whatever order places are first called in, as threads working at
//! once decide --
//! `site.operation` only the calls that make that operation (`fs.hard_link`,
//! where the caller names what it does), and
//! `site:E` (or `site:E=N`, `site:E@N`) fails it with errno `E` instead of
//! its usual error. An entry written `__command/site...` applies only in a
//! process started as `supercov __command` -- a doctest's or nextest's runner
//! -- and one written `env:NAME/site...` only in a process the variable
//! `NAME` is set for -- a merged doctest's child -- so a test can fail its
//! Nth place there and not its parent's. A failing
//! site does not make its call; it returns what the call returns when it
//! fails, and says so on standard error (`test fault at <site>`), so a test
//! knows the site was reached.
//!
//! A test names a place, never its file and line: an edit above a call
//! moves its line, and a test that pinned it failed for it, far from the
//! edit. So a place a test fails is named where it is, on the line above
//! the call (or above the call of a `#[track_caller]` helper that reaches
//! the site, which is where the place is):
//!
//! ```text
//! // fault-site: run-store.discover-runs
//! for entry in fs::read_dir(&store)? {
//! ```
//!
//! and `fs.read_dir#run-store.discover-runs` fails it. A name is lowercase
//! words joined by `-` and `.`, with a `.` at least: the file's area, then
//! what the call does there. Each crate's build script collects its names and
//! fails the build when one is given twice; `scripts/check-fault-sites.mjs`
//! (the lint job) holds them unique across crates, fails a test or script
//! that names a site no comment gives, and fails a line pin. The message a
//! failing place writes is `test fault at fs (NAME operation) at file.rs:L`:
//! a test matches the name and operation, never the file and line after
//! them, which are for a reader. Only debug builds read the variable: a release
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
    /// For `site#...`: the place it names. The calls from that place are
    /// counted by `skip` and `remaining`.
    place: Option<Named>,
}

/// The place a `site#...` names.
#[cfg(any(test, debug_assertions))]
enum Named {
    /// The Nth place the site was called from, and those places so far, in
    /// the order they first called it.
    Nth(usize, std::sync::Mutex<Vec<Place>>),
    /// The place a `// fault-site: NAME` comment names.
    Site(&'static Site),
}

/// Where in the code a call was made, and which call it was: a helper marked
/// `#[track_caller]` hands its caller's place to the calls it makes, and two
/// different calls from there are two places.
pub type Place = (&'static std::panic::Location<'static>, &'static str);

/// A call site named for tests: its name, its file's path under the crate's
/// `src`, and its line. Each crate's build script finds the comments
/// `// fault-site: NAME` in its sources and writes the table of them.
pub type Site = (&'static str, &'static str, u32);

/// The engine's named sites.
#[cfg(any(test, debug_assertions))]
const SITES: &[Site] = include!(concat!(env!("OUT_DIR"), "/fault_sites.rs"));

/// Another crate's named sites, [`name_sites`] gave.
#[cfg(any(test, debug_assertions))]
static MORE_SITES: std::sync::OnceLock<&'static [Site]> = std::sync::OnceLock::new();

/// Name the sites of the crate that calls this, its build script's table:
/// the CLI's, first thing in `main`, before a fault is read.
pub fn name_sites(sites: &'static [Site]) {
    #[cfg(any(test, debug_assertions))]
    let _ = MORE_SITES.set(sites);
    #[cfg(not(any(test, debug_assertions)))]
    let _ = sites;
}

/// Every named site.
#[cfg(any(test, debug_assertions))]
fn sites() -> impl Iterator<Item = &'static Site> {
    SITES
        .iter()
        .chain(MORE_SITES.get().into_iter().copied().flatten())
}

/// The site named `name`. A name no site has is a test's mistake: it would
/// fail nothing, so it stops the test here instead.
#[cfg(any(test, debug_assertions))]
fn site_named(name: &str) -> &'static Site {
    sites()
        .find(|site| site.0 == name)
        .unwrap_or_else(|| panic!("no fault site is named `{name}`"))
}

/// Whether `file`, a path as `file!()` writes it, is the source at
/// `relative` under a crate's `src`: it ends with that path, after a
/// separator of either kind.
#[cfg(any(test, debug_assertions))]
fn is_source(file: &str, relative: &str) -> bool {
    let Some(start) = file.len().checked_sub(relative.len()) else {
        return false;
    };
    let same = |(&ours, &theirs): (&u8, &u8)| ours == theirs || ours == b'\\' && theirs == b'/';
    file.as_bytes()[start..]
        .iter()
        .zip(relative.as_bytes())
        .all(same)
        && file.as_bytes()[..start]
            .last()
            .is_none_or(|&before| before == b'/' || before == b'\\')
}

/// The name of the site at `location`, if it has one.
#[cfg(any(test, debug_assertions))]
fn name_at(location: &std::panic::Location<'_>) -> Option<&'static str> {
    sites()
        .find(|(_, file, line)| *line == location.line() && is_source(location.file(), file))
        .map(|(name, _, _)| *name)
}

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
            // `site@N` the Nth call, `site#N@M` the Mth from the Nth place.
            let (place, nth) = place.split_once('@').unwrap_or((place, ""));
            let (site, nth) = site.split_once('@').unwrap_or((site, nth));
            let (site, errno) = site.split_once(':').unwrap_or((site, ""));
            let nth = nth.parse::<usize>().ok();
            Fault {
                site: site.to_owned(),
                errno: errno.parse().ok(),
                skip: nth.map_or(0, |nth| nth.saturating_sub(1)).into(),
                // A place's first call when no other is named.
                remaining: nth
                    .map(|_| 1)
                    .or_else(|| count.parse().ok())
                    .or_else(|| (!place.is_empty()).then_some(1))
                    .map(std::sync::atomic::AtomicUsize::new),
                place: (!place.is_empty()).then(|| match place.parse::<usize>() {
                    Ok(nth) => Named::Nth(nth, std::sync::Mutex::new(Vec::new())),
                    Err(_) => Named::Site(site_named(place)),
                }),
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
            .inspect(|_| announce(site, place))
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        let _ = (site, place);
        None
    }
}

/// Say that the call at `site` failed by request, and, when the site knows
/// where the call was made, the name of that place and the operation, then
/// its file and line for a reader
/// (`test fault at fs (run-store.discover-runs read_dir) at run_store.rs:L`),
/// so a test can check it failed the call it meant to. A place no comment
/// names gives the operation alone before its file and line.
/// Written to the descriptor itself: a child forked to exec a command may not
/// allocate or take the lock `eprintln!` takes, so the line is written as
/// digits from the stack. Windows writes to the handle for the same reason:
/// the run holds standard error's lock for its diagnostics while it waits on
/// the thread a fault fires on, and `eprint!` there waited for it for good.
#[cfg(any(test, debug_assertions))]
fn announce(site: &str, place: Option<Place>) {
    let mut digits = [0_u8; 10];
    let mut first = digits.len();
    let (file, operation, name) = match place {
        Some((location, operation)) => {
            let mut line = location.line();
            // A line has a digit at least.
            loop {
                first -= 1;
                digits[first] = b'0' + (line % 10) as u8;
                line /= 10;
                if line == 0 {
                    break;
                }
            }
            let file = location.file();
            (
                file.rsplit(['/', '\\']).next().unwrap_or(file),
                operation,
                name_at(location),
            )
        }
        None => ("", "", None),
    };
    let (name, space): (&[u8], &[u8]) = match name {
        Some(name) => (name.as_bytes(), b" "),
        None => (b"", b""),
    };
    let placed: [&[u8]; 9] = match place {
        Some(_) => [
            b" (",
            name,
            space,
            operation.as_bytes(),
            b") at ",
            file.as_bytes(),
            b":",
            &digits[first..],
            b"\n",
        ],
        None => [b"", b"", b"", b"", b"", b"", b"", b"", b"\n"],
    };
    #[cfg(unix)]
    [b"[supercov] test fault at ".as_slice(), site.as_bytes()]
        .into_iter()
        .chain(placed)
        .for_each(|part| {
            // SAFETY: a write of a live buffer to standard error.
            unsafe { libc::write(2, part.as_ptr().cast(), part.len()) };
        });
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        // SAFETY: this process's standard error, borrowed for the write and
        // never closed here.
        let mut stderr = std::mem::ManuallyDrop::new(unsafe {
            std::fs::File::from_raw_handle(std::io::stderr().as_raw_handle())
        });
        [b"[supercov] test fault at ".as_slice(), site.as_bytes()]
            .into_iter()
            .chain(placed)
            .for_each(|part| {
                let _ = stderr.write_all(part);
            });
    }
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
    // fails the third and the seventh. `site.operation` names only the calls
    // that make that operation: `fs.hard_link` fails each hard link.
    let named_here = |fault: &&Fault| {
        fault.site == site
            || place.is_some_and(|(_, operation)| {
                fault
                    .site
                    .strip_prefix(site)
                    .and_then(|rest| rest.strip_prefix('.'))
                    == Some(operation)
            })
    };
    let mut named = false;
    let mut failed = None;
    for fault in faults.iter().filter(named_here) {
        named = true;
        let fails = match (&fault.place, place) {
            // A call from the Nth place the site was called from, counted
            // as `site@N` counts every call: the first, or the Mth.
            (Some(Named::Nth(nth, seen)), Some(place)) => {
                let mut seen = seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !seen.contains(&place) {
                    seen.push(place);
                }
                nth.checked_sub(1).and_then(|at| seen.get(at)) == Some(&place)
                    && !count_down(&fault.skip)
                    && fault.remaining.as_ref().is_none_or(count_down)
            }
            // A call from the place a comment names.
            (Some(Named::Site((_, file, line))), Some((location, _))) => {
                location.line() == *line
                    && is_source(location.file(), file)
                    && !count_down(&fault.skip)
                    && fault.remaining.as_ref().is_none_or(count_down)
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

/// Standard error, for a run's diagnostics: each line written is a call at
/// the site `diagnostics`, so `diagnostics@N` fails the run's Nth line.
///
/// Each line takes std's lock on standard error as it is written, and gives
/// it back. Held for the run, it kept every other thread from writing there:
/// one that wrote a line (`eprintln!`) waited for the run to end, and a run
/// that joined it never did.
pub fn diagnostics() -> Writer<io::Stderr> {
    Writer {
        site: "diagnostics",
        inner: io::stderr(),
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

    /// A line as one call: `writeln!` writes its parts one at a time.
    fn write_fmt(&mut self, arguments: std::fmt::Arguments<'_>) -> io::Result<()> {
        io(self.site, io::ErrorKind::BrokenPipe, || {
            self.inner.write_fmt(arguments)
        })
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

#[cfg(test)]
mod tests {
    /// Another thread writes to standard error while a run's diagnostics are
    /// open, and the run waits for that thread: it must not wait forever.
    #[test]
    fn diagnostics_leave_standard_error_to_other_threads() {
        use std::io::Write as _;
        let mut diagnostics = super::diagnostics();
        writeln!(diagnostics).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = std::io::stderr().lock().write_all(b"");
            let _ = sender.send(());
        });
        let wrote = receiver.recv_timeout(std::time::Duration::from_secs(10));
        writeln!(diagnostics).unwrap();
        assert!(
            wrote.is_ok(),
            "a thread writing to standard error waited for the run's diagnostics"
        );
    }

    #[track_caller]
    fn here() -> bool {
        super::injected_here("t", "op")
    }

    /// `site.operation` fails the calls that make that operation, and no
    /// other: a call that names another goes on.
    #[test]
    fn an_operation_is_failed_by_its_name() {
        let other = || super::injected_here("t", "other");
        let (named, unnamed) = super::during("t.op", || (here(), other()));
        assert!(named);
        assert!(!unnamed);
    }

    /// `site#N` fails the first call from the Nth place only: an earlier
    /// place, and a place called again, go on as usual.
    #[test]
    fn the_nth_place_fails_its_first_call() {
        let (first, second, again) = super::during("t#2", || {
            let first = here();
            let calls = (0..2).map(|_| here()).collect::<Vec<_>>();
            (first, calls[0], calls[1])
        });
        assert!(!first);
        assert!(second);
        assert!(!again);
    }

    /// `site#NAME` fails the call at the place a `// fault-site: NAME`
    /// comment names, and with `@M` its Mth; a call elsewhere goes on, and so
    /// does one that cannot say where it is.
    #[test]
    fn a_place_named_by_a_comment_fails_there() {
        let call = || {
            // fault-site: fault.named-for-a-test
            super::injected_here("t", "op")
        };
        let calls = super::during("t#fault.named-for-a-test@2", || {
            (0..3).map(|_| call()).collect::<Vec<_>>()
        });
        assert_eq!(calls, [false, true, false]);
        let elsewhere = super::during("t#fault.named-for-a-test", || {
            super::injected_here("t", "op")
        });
        assert!(!elsewhere);
        let unplaced = super::during("t#fault.named-for-a-test", || super::injected("t"));
        assert!(!unplaced);
    }

    /// A name no site has stops the test that wrote it.
    #[test]
    #[should_panic(expected = "no fault site is named `fault.nowhere`")]
    fn a_name_no_site_has_is_refused() {
        super::during("t#fault.nowhere", || ());
    }

    /// A named place's file is the source under `src` it names, whichever
    /// separator the build wrote.
    #[test]
    fn a_named_file_is_matched_by_its_path_under_src() {
        for (file, relative, expected) in [
            ("crates/supercov-engine/src/fault.rs", "fault.rs", true),
            ("fault.rs", "fault.rs", true),
            (
                "crates\\supercov-cli\\src\\quality\\store.rs",
                "quality/store.rs",
                true,
            ),
            ("src/unfault.rs", "fault.rs", false),
            ("rs", "fault.rs", false),
            ("src/quality\\store.rs", "quality_store.rs", false),
            ("src/quality/other.rs", "quality/store.rs", false),
        ] {
            assert_eq!(
                super::is_source(file, relative),
                expected,
                "{file} {relative}"
            );
        }
    }

    /// `site#N@M` fails the Mth call from the Nth place only.
    #[test]
    fn the_nth_place_fails_its_mth_call() {
        let calls = super::during("t#2@3", || {
            let first = here();
            let mut calls = vec![first];
            calls.extend((0..4).map(|_| here()));
            calls
        });
        assert_eq!(calls, [false, false, false, true, false]);
    }
}
