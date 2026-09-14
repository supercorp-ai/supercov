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
