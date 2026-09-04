//! Language/frontend selection for the zero-configuration public command.
//!
//! The command launch intent is authoritative; project manifests provide a
//! fallback for opaque wrappers. Multiple frontends may be selected for a
//! genuinely mixed test command. No language is guessed from source extension
//! alone.

use std::{collections::BTreeSet, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::project_discovery::expanded_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontendLanguage {
    JavaScript,
    Python,
    Ruby,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendEvidence {
    pub language: FrontendLanguage,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendDetection {
    pub frontends: Vec<FrontendLanguage>,
    pub evidence: Vec<FrontendEvidence>,
}

fn tokens(value: &str) -> Vec<String> {
    value
        .to_ascii_lowercase()
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
        })
        .filter(|value| !value.is_empty())
        .map(|value| {
            Path::new(value)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(value)
                .trim_end_matches(".exe")
                .trim_end_matches(".cmd")
                .to_owned()
        })
        .collect()
}

fn has_sequence(tokens: &[String], sequence: &[&str]) -> bool {
    tokens.windows(sequence.len()).any(|window| {
        window
            .iter()
            .map(String::as_str)
            .eq(sequence.iter().copied())
    })
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn supported_by_command(command_tokens: &[String]) -> Vec<(FrontendLanguage, &'static str)> {
    let mut launched = Vec::new();
    let rust_command = has_sequence(command_tokens, &["cargo", "test"])
        || has_sequence(command_tokens, &["cargo", "nextest"])
        || has_sequence(command_tokens, &["cargo-nextest", "run"])
        || has_sequence(command_tokens, &["cross", "test"]);
    if rust_command {
        launched.push((
            FrontendLanguage::Rust,
            "the expanded test command launches Cargo's test pipeline",
        ));
    }

    let python_command = command_tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "pytest" | "py.test" | "unittest" | "tox" | "nox"
        )
    });
    if python_command {
        launched.push((
            FrontendLanguage::Python,
            "the expanded test command launches a Python test runner",
        ));
    }
    let ruby_command = command_tokens
        .iter()
        .any(|token| matches!(token.as_str(), "rspec" | "minitest" | "cucumber" | "m"))
        || has_sequence(command_tokens, &["rake", "spec"])
        || has_sequence(command_tokens, &["rake", "test"])
        || has_sequence(command_tokens, &["rails", "test"])
        || (command_tokens.iter().any(|token| token == "ruby")
            && command_tokens.iter().any(|token| {
                token.ends_with("_spec.rb")
                    || token.ends_with("_test.rb")
                    || token.starts_with("-itest")
                    || token.starts_with("-ispec")
            }));
    if ruby_command {
        launched.push((
            FrontendLanguage::Ruby,
            "the expanded test command launches a Ruby test runner",
        ));
    }

    let javascript_command = command_tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "playwright" | "vitest" | "jest" | "node" | "tsx" | "ts-node"
        )
    }) || command_tokens
        .iter()
        .any(|token| token.starts_with("playwright-"));
    if javascript_command {
        launched.push((
            FrontendLanguage::JavaScript,
            "the expanded test command launches a JavaScript test/runtime process",
        ));
    }
    launched
}

pub fn detect_frontends(root: &Path, command: &[String]) -> FrontendDetection {
    let expanded = expanded_command(root, command);
    let command_tokens = tokens(&expanded);
    let mut selected = BTreeSet::new();
    let mut evidence = Vec::new();

    for (language, reason) in supported_by_command(&command_tokens) {
        selected.insert(language);
        evidence.push(FrontendEvidence {
            language,
            reason: reason.into(),
        });
    }

    // Opaque wrappers such as `make test` or `./scripts/test` do not reveal
    // their child graph before launch. Instrument every strongly evidenced
    // project frontend in that case; the eventual launch observer validates
    // which prepared frontend actually ran.
    if selected.is_empty() {
        let candidates = [
            (
                FrontendLanguage::Rust,
                regular_file(&root.join("Cargo.toml")),
                "Cargo.toml exists and the test command is opaque",
            ),
            (
                FrontendLanguage::Ruby,
                ["Gemfile", ".rspec", "Rakefile"]
                    .iter()
                    .any(|name| root.join(name).is_file())
                    && (root.join("spec").is_dir() || root.join("test").is_dir()),
                "Ruby project/test metadata exists and the test command is opaque",
            ),
            (
                FrontendLanguage::Python,
                ["pyproject.toml", "pytest.ini", "tox.ini"]
                    .iter()
                    .any(|name| regular_file(&root.join(name))),
                "Python project/test metadata exists and the test command is opaque",
            ),
            (
                FrontendLanguage::JavaScript,
                regular_file(&root.join("package.json")),
                "package.json exists and the test command is opaque",
            ),
        ];
        for (language, present, reason) in candidates {
            if present {
                selected.insert(language);
                evidence.push(FrontendEvidence {
                    language,
                    reason: reason.into(),
                });
            }
        }
    }

    FrontendDetection {
        frontends: selected.into_iter().collect(),
        evidence,
    }
}

/// A language Supercov recognizes but cannot measure yet. Naming it turns
/// "could not determine a supported test language" into an answer the user
/// can act on: what was detected, from which signal, and where to ask for
/// (or contribute) support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedEcosystem {
    pub language: &'static str,
    pub evidence: String,
    /// True when the test command itself launches the unsupported runner.
    /// Command intent is authoritative: `supercov -- go test` deserves the
    /// Go answer even in a repository that also carries a package.json,
    /// because the manifest fallback exists only for opaque commands.
    pub from_command: bool,
}

/// Whether the expanded test command itself launches a runner Supercov
/// supports. Used to keep command-authoritative unsupported detection from
/// shadowing genuinely mixed commands like `sh -c "go test && vitest run"`.
pub fn command_launches_supported_frontend(root: &Path, command: &[String]) -> bool {
    let expanded = expanded_command(root, command);
    let command_tokens = tokens(&expanded);
    supported_by_command(&command_tokens)
        .into_iter()
        .next()
        .is_some()
}

pub fn detect_unsupported_ecosystem(
    root: &Path,
    command: &[String],
) -> Option<UnsupportedEcosystem> {
    let expanded = expanded_command(root, command);
    let command_tokens = tokens(&expanded);
    let by_command: &[(&str, &[&str])] = &[
        ("Go", &["go"]),
        ("Java/Kotlin", &["mvn", "maven", "gradle", "gradlew"]),
        ("PHP", &["phpunit", "pest"]),
        (".NET", &["dotnet"]),
        ("Elixir", &["mix"]),
        ("Swift", &["swift"]),
        ("Dart/Flutter", &["flutter", "dart"]),
    ];
    for (language, runners) in by_command {
        for runner in *runners {
            // `go test`, `swift test`, `mix test`, `dotnet test`, `gradle
            // test`: the runner word alone is too common in shell text, so
            // require the test-launch shape for single-word runners.
            let launches = if matches!(*runner, "go" | "swift" | "mix" | "dotnet") {
                has_sequence(&command_tokens, &[runner, "test"])
            } else {
                command_tokens.iter().any(|token| token == runner)
            };
            if launches {
                return Some(UnsupportedEcosystem {
                    language,
                    evidence: format!("the test command runs `{runner}`"),
                    from_command: true,
                });
            }
        }
    }
    let by_manifest: &[(&str, &[&str])] = &[
        ("Go", &["go.mod"]),
        (
            "Java/Kotlin",
            &["pom.xml", "build.gradle", "build.gradle.kts"],
        ),
        ("PHP", &["composer.json"]),
        ("Elixir", &["mix.exs"]),
        ("Swift", &["Package.swift"]),
        ("Dart/Flutter", &["pubspec.yaml"]),
    ];
    for (language, manifests) in by_manifest {
        for manifest in *manifests {
            if regular_file(&root.join(manifest)) {
                return Some(UnsupportedEcosystem {
                    language,
                    evidence: format!("{manifest} is present"),
                    from_command: false,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("supercov-detection-{}-{name}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn direct_cargo_is_authoritative_even_in_a_polyglot_repository() {
        let root = fixture("cargo");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\n",
        )
        .unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .unwrap();
        let detected = detect_frontends(&root, &["cargo".into(), "test".into()]);
        assert_eq!(detected.frontends, [FrontendLanguage::Rust]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_script_expansion_finds_cargo_without_hardcoding_the_repository() {
        let root = fixture("npm-cargo");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test:rust":"cargo test --workspace"}}"#,
        )
        .unwrap();
        let detected = detect_frontends(&root, &["npm".into(), "run".into(), "test:rust".into()]);
        assert_eq!(detected.frontends, [FrontendLanguage::Rust]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixed_shell_commands_select_both_real_frontends() {
        let root = fixture("mixed");
        let detected = detect_frontends(
            &root,
            &[
                "sh".into(),
                "-c".into(),
                "cargo test && npx vitest run".into(),
            ],
        );
        assert_eq!(
            detected.frontends,
            [FrontendLanguage::JavaScript, FrontendLanguage::Rust]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_known_unsupported_runner_is_named_from_the_command() {
        let root = fixture("go-command");
        let detected = detect_frontends(&root, &["go".into(), "test".into(), "./...".into()]);
        assert_eq!(detected.frontends, []);
        let ecosystem =
            detect_unsupported_ecosystem(&root, &["go".into(), "test".into(), "./...".into()])
                .unwrap();
        assert_eq!(ecosystem.language, "Go");
        assert_eq!(ecosystem.evidence, "the test command runs `go`");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_known_unsupported_manifest_is_named_when_the_command_is_opaque() {
        let root = fixture("gomod");
        fs::write(root.join("go.mod"), "module example.com/app\n").unwrap();
        let detected = detect_frontends(&root, &["make".into(), "test".into()]);
        assert_eq!(detected.frontends, []);
        let ecosystem =
            detect_unsupported_ecosystem(&root, &["make".into(), "test".into()]).unwrap();
        assert_eq!(ecosystem.language, "Go");
        assert_eq!(ecosystem.evidence, "go.mod is present");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ruby_runners_and_manifests_select_the_ruby_frontend() {
        let root = fixture("ruby");
        fs::write(root.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
        fs::create_dir_all(root.join("spec")).unwrap();
        for command in [
            vec!["rspec".to_string()],
            vec!["bundle".into(), "exec".into(), "rspec".into()],
            vec!["ruby".into(), "-Itest".into(), "test/app_test.rb".into()],
            vec!["bin/rails".into(), "test".into()],
            vec!["make".into(), "test".into()],
        ] {
            let detected = detect_frontends(&root, &command);
            assert_eq!(detected.frontends, [FrontendLanguage::Ruby], "{command:?}");
        }
        assert!(detect_unsupported_ecosystem(&root, &["make".into(), "test".into()]).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_explicit_unsupported_command_is_authoritative_over_manifests() {
        let root = fixture("go-with-package-json");
        fs::write(root.join("package.json"), "{}").unwrap();
        fs::write(root.join("go.mod"), "module example.com/x\n").unwrap();
        let command = vec!["go".into(), "test".into(), "./...".into()];
        let ecosystem = detect_unsupported_ecosystem(&root, &command).unwrap();
        assert!(ecosystem.from_command);
        assert_eq!(ecosystem.language, "Go");
        assert!(!command_launches_supported_frontend(&root, &command));
        // A genuinely mixed command that also launches a supported runner
        // must keep running.
        let mixed = vec!["sh".into(), "-c".into(), "go test && npx vitest run".into()];
        assert!(command_launches_supported_frontend(&root, &mixed));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supported_and_unknown_projects_stay_unnamed() {
        let root = fixture("unknown");
        assert_eq!(
            detect_unsupported_ecosystem(&root, &["cargo".into(), "test".into()]),
            None
        );
        assert_eq!(
            detect_unsupported_ecosystem(&root, &["./scripts/test".into()]),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opaque_commands_prepare_all_manifest_backed_frontends() {
        let root = fixture("opaque");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        fs::write(root.join("pyproject.toml"), "[project]\nname='fixture'\n").unwrap();
        let detected = detect_frontends(&root, &["make".into(), "test".into()]);
        assert_eq!(
            detected.frontends,
            [
                FrontendLanguage::JavaScript,
                FrontendLanguage::Python,
                FrontendLanguage::Rust
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
