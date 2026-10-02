//! `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL` against a local server that
//! answers the way a gateway does: at its own path, with a dated build of the
//! model it was asked for.
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::Value;

const DATED_BUILD: &str = "typesafe/jev-1.13-20260917";

/// What the server was sent: the path, the Authorization header and the body.
type Seen = common::Seen;

/// Answers every question it is asked, as `model`, and records each request.
fn gateway(model: &'static str) -> (String, Seen) {
    common::gateway(model, common::answer_no)
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "supercov-typesafe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"p","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("src/add.js"),
            "export function add(a, b) {\n  return a + b;\n}\n",
        )
        .unwrap();
        Self(root)
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn supercov(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_supercov"));
    command.args(args).current_dir(root);
    for name in [
        "TYPESAFE_API_KEY",
        "TYPESAFE_BASE_URL",
        "TYPESAFE_DEFAULT_MODEL",
    ] {
        command.env_remove(name);
    }
    command.envs(env.iter().copied()).output().unwrap()
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "no JSON report\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn a_gateway_serves_both_catalogs_with_the_model_it_names() {
    let (base, seen) = gateway(DATED_BUILD);
    let project = Project::new();
    let env = [
        ("TYPESAFE_BASE_URL", base.as_str()),
        ("TYPESAFE_DEFAULT_MODEL", "typesafe/jev-1.13"),
        ("TYPESAFE_API_KEY", "sk-gateway"),
    ];
    for command in ["quality", "security"] {
        let output = supercov(&project.0, &[command, "src/add.js", "--json"], &env);
        let report = report(&output);
        assert!(output.status.success(), "{command}: {report}");
        assert_eq!(report["model"], "typesafe/jev-1.13", "{command}");
        assert_eq!(report["counts"]["errors"], 0, "{command}: {report}");
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (path, authorization, request) in seen.iter() {
        assert_eq!(path, "/api/v1/systemone");
        assert_eq!(authorization, "Bearer sk-gateway");
        assert_eq!(request["model"], "typesafe/jev-1.13");
    }
}

#[test]
fn answers_are_cached_per_model() {
    let (base, seen) = gateway(DATED_BUILD);
    let project = Project::new();
    let run = |model: &str| {
        let env = [
            ("TYPESAFE_BASE_URL", base.as_str()),
            ("TYPESAFE_DEFAULT_MODEL", model),
            ("TYPESAFE_API_KEY", "sk-gateway"),
        ];
        let output = supercov(&project.0, &["quality", "src/add.js", "--json"], &env);
        assert!(output.status.success(), "{}", report(&output));
    };
    run("typesafe/jev-1.13");
    run("typesafe/jev-1.13");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the repeat is answered from cache"
    );
    run("~typesafe/jev-latest");
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "another model is asked again"
    );
}

#[test]
fn the_default_model_must_answer_as_itself() {
    let (base, seen) = gateway(DATED_BUILD);
    let project = Project::new();
    let env = [
        ("TYPESAFE_BASE_URL", base.as_str()),
        ("TYPESAFE_API_KEY", "sk-gateway"),
    ];
    let output = supercov(&project.0, &["quality", "src/add.js", "--json"], &env);
    let report = report(&output);
    assert_eq!(seen.lock().unwrap()[0].2["model"], "jev-1.13.0");
    assert_eq!(report["model"], "jev-1.13.0");
    assert_eq!(report["counts"]["errors"], 1, "{report}");
    assert!(
        report.to_string().contains("expected model jev-1.13.0"),
        "{report}"
    );
}

#[test]
fn a_key_is_never_sent_in_the_clear_to_another_machine() {
    let project = Project::new();
    let env = [
        ("TYPESAFE_BASE_URL", "http://gateway.example.com/api"),
        ("TYPESAFE_API_KEY", "sk-gateway"),
    ];
    let output = supercov(&project.0, &["quality", "src/add.js"], &env);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TYPESAFE_BASE_URL must be an https:// URL"),
        "{stderr}"
    );
}
