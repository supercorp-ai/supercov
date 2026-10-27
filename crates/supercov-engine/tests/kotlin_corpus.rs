//! Every Kotlin file of a directory of real projects, parsed with the grammar
//! Supercov instruments with: run by `scripts/kotlin-corpus.mjs`, which checks
//! out the projects and compares what fails with the files known to.

use std::path::{Path, PathBuf};

use supercov_engine::jvm_instrumenter::{JvmLanguage, parse};

fn kotlin_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            kotlin_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "kt") {
            files.push(path);
        }
    }
}

#[test]
#[ignore = "parses checked-out projects; run it with `node scripts/kotlin-corpus.mjs`"]
fn every_file_of_the_corpus_parses_or_is_known_not_to() {
    let corpus = PathBuf::from(std::env::var_os("SUPERCOV_KOTLIN_CORPUS").expect("corpus"));
    let report = PathBuf::from(std::env::var_os("SUPERCOV_KOTLIN_REPORT").expect("report"));
    let mut files = Vec::new();
    kotlin_files(&corpus, &mut files);
    files.sort();
    let mut failures = Vec::new();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        if let Err(error) = parse(&source, JvmLanguage::Kotlin) {
            let relative = file.strip_prefix(&corpus).unwrap_or(file);
            failures.push(format!(
                "{}\t{}",
                relative.to_string_lossy().replace('\\', "/"),
                error.to_string().replace(['\n', '\t'], " ")
            ));
        }
    }
    std::fs::write(&report, format!("{}\n{}", files.len(), failures.join("\n"))).expect("report");
}
