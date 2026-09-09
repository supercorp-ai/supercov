//! Exercise the public command without requiring a test run or a JS compiler.
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_supercov"))
        .args(args)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run the real CLI")
}

fn successful_text(args: &[&str]) -> String {
    let output = cli(args);
    assert!(output.status.success(), "{:?}", output);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn assertion_query_is_discoverable_without_a_run_or_compiler() {
    assert!(successful_text(&["--help"]).contains("runs latest assertions"));
    for flag in ["--help", "-h"] {
        let listing = successful_text(&["runs", "latest", flag]);
        assert!(listing.contains("\n  assertions "));
        assert!(!listing.contains("\n  asserted "));
        assert!(!listing.to_lowercase().contains("experimental"));
        let help = successful_text(&["runs", "latest", "assertions", flag]);
        assert!(help.starts_with("Usage: supercov runs <run-id> assertions "));
        assert!(help.contains("source behaviors"));
        assert!(help.contains("not a proven assertion score"));
        assert!(!help.to_lowercase().contains("experimental"));
        for option in ["--pragmas", "--evidence", "--analysis", "--file", "--site"] {
            assert!(help.contains(option), "{option}");
        }
    }
    assert!(successful_text(&["docs"]).contains("\n  assertion-evidence\n"));
}

#[test]
fn assertion_option_errors_use_the_public_json_command_name() {
    for options in [vec!["--unknown"], vec!["--limit", "0"]] {
        let mut args = vec!["runs", "latest", "assertions", "--json"];
        args.extend(options);
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2));
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["command"], "coverage.assertions");
        assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
    }
}

#[test]
fn retired_unreleased_name_is_not_a_second_public_command() {
    let output = cli(&["runs", "latest", "asserted", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "UNKNOWN_COMMAND");
}
