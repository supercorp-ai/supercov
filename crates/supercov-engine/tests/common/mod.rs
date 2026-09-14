//! Shared by the integration tests that need a language toolchain.

/// Record that a test is being skipped, or fail if this environment promised
/// the toolchain would be there.
///
/// These tests skip on a machine without the toolchain, so a contributor
/// without Go or a JDK can still run the suite. That is exactly the property
/// that makes them worthless in CI unless something checks: a job whose
/// toolchain setup silently failed would skip every test and report success,
/// and the language would look verified while nothing had run.
///
/// CI sets `SUPERCOV_REQUIRE_GO=1` or `SUPERCOV_REQUIRE_JVM=1`, which turns
/// the skip into the failure it should be there.
pub fn skip(language: &str, reason: &str) {
    let variable = format!("SUPERCOV_REQUIRE_{}", language.to_ascii_uppercase());
    if std::env::var(&variable).as_deref() == Ok("1") {
        panic!(
            "{variable} is set, so this test must run rather than skip, and it cannot: {reason}"
        );
    }
    eprintln!("[{language}] skipped: {reason}");
}

/// Find a build tool or compiler, or `None` on a machine without it.
///
/// More than a PATH lookup, for two reasons. Homebrew's installs are not on a
/// non-login shell's PATH on macOS, which is where this suite is usually
/// developed. And on Windows `mvn` and `gradle` are `.cmd` scripts: Rust's
/// `Command` resolves a bare name to `.exe` and nothing else, so asking for
/// `mvn` there finds nothing at all and every JVM test would skip on the one
/// platform whose differences most deserve a test.
#[allow(dead_code)]
pub fn tool(name: &str) -> Option<std::path::PathBuf> {
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".cmd", ".bat", ".exe"]
    } else {
        &[""]
    };
    let prefixes = [
        "",
        "/opt/homebrew/bin/",
        "/opt/homebrew/opt/openjdk/bin/",
        "/usr/local/bin/",
        "/usr/local/go/bin/",
        "/usr/bin/",
    ];
    for prefix in prefixes {
        for extension in extensions {
            let path = std::path::PathBuf::from(format!("{prefix}{name}{extension}"));
            // `--version` for most, `-version` for the JDK's own tools.
            for flag in ["--version", "-version"] {
                if std::process::Command::new(&path)
                    .arg(flag)
                    .output()
                    .is_ok_and(|out| out.status.success())
                {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Whether the toolchain can build the fixture, deciding it by trying only
/// when the answer could change what happens.
///
/// These tests build the fixture once to learn whether its dependencies
/// resolve here, and skip when they do not — a cold cache with no network is a
/// real situation on someone's laptop. In CI it is not: `SUPERCOV_REQUIRE_*`
/// says skipping is forbidden, so a fixture that cannot build must fail the
/// job whether it is discovered now or a moment later. Asking anyway doubles
/// every Maven and Gradle invocation in the suite for an answer that cannot be
/// acted on.
#[allow(dead_code)]
pub fn resolvable(language: &str, probe: impl FnOnce() -> bool) -> bool {
    let variable = format!("SUPERCOV_REQUIRE_{}", language.to_ascii_uppercase());
    if std::env::var(&variable).as_deref() == Ok("1") {
        return true;
    }
    probe()
}

/// A classpath from its entries.
///
/// The separator is a colon everywhere except Windows, where it is a
/// semicolon — a colon there is read as part of a drive letter, so every entry
/// after the first is lost and the compiler reports a package that plainly
/// exists as missing.
#[allow(dead_code)]
pub fn classpath(entries: &[&str]) -> String {
    entries.join(if cfg!(windows) { ";" } else { ":" })
}

/// The major Java release a compiler supports, from its own `-version`.
///
/// A fixture may need a language feature older compilers do not have — record
/// patterns are Java 21 — and that is not the same as the toolchain being
/// absent. The require flag exists to catch a missing toolchain; a present one
/// that is simply older should skip the cases it cannot express and run the
/// rest.
#[allow(dead_code)]
pub fn java_release(javac: &std::path::Path) -> Option<u32> {
    let output = std::process::Command::new(javac)
        .arg("-version")
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.split_whitespace()
        .find_map(|word| word.split('.').next()?.parse::<u32>().ok())
}

/// Serialise the tests that drive a real Maven or Gradle build.
///
/// `npm run test:jvm` passes `--test-threads=1`, which says plainly that these
/// cannot run alongside each other -- but `cargo test --workspace` does not,
/// and three CI jobs plus `release:check` use exactly that. On macOS and Linux
/// the parallel run is merely faster; on Windows it stops. The filesystem job
/// went from eleven minutes on main to over seventy on the branch that added
/// these tests, on both Windows runners, while macOS ran the same command in
/// eleven and Linux in seven.
///
/// A flag on one script cannot protect a command someone else runs, so the
/// rule belongs in the test rather than in the way it is invoked. Cargo runs
/// test binaries one at a time, so a lock per process is enough.
///
/// A panicking test poisons the mutex; the guard is taken anyway, because a
/// failed test must not turn every later one into a second failure that hides
/// it.
#[allow(dead_code)]
pub fn building() -> std::sync::MutexGuard<'static, ()> {
    static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    BUILD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
