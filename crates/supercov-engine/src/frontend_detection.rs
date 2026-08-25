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

pub fn detect_frontends(root: &Path, command: &[String]) -> FrontendDetection {
    let expanded = expanded_command(root, command);
    let command_tokens = tokens(&expanded);
    let mut selected = BTreeSet::new();
    let mut evidence = Vec::new();

    let rust_command = has_sequence(&command_tokens, &["cargo", "test"])
        || has_sequence(&command_tokens, &["cargo", "nextest"])
        || has_sequence(&command_tokens, &["cargo-nextest", "run"])
        || has_sequence(&command_tokens, &["cross", "test"]);
    if rust_command {
        selected.insert(FrontendLanguage::Rust);
        evidence.push(FrontendEvidence {
            language: FrontendLanguage::Rust,
            reason: "the expanded test command launches Cargo's test pipeline".into(),
        });
    }

    let python_command = command_tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "pytest" | "py.test" | "unittest" | "tox" | "nox"
        )
    });
    if python_command {
        selected.insert(FrontendLanguage::Python);
        evidence.push(FrontendEvidence {
            language: FrontendLanguage::Python,
            reason: "the expanded test command launches a Python test runner".into(),
        });
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
        selected.insert(FrontendLanguage::JavaScript);
        evidence.push(FrontendEvidence {
            language: FrontendLanguage::JavaScript,
            reason: "the expanded test command launches a JavaScript test/runtime process".into(),
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
