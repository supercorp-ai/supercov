use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        files.push(path.to_owned());
        return;
    }
    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .map(|entry| entry.expect("failed to read build-input entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_files(&entry, files);
        } else if entry.is_file() {
            files.push(entry);
        }
    }
}

fn digest_files(files: &[PathBuf], workspace_root: &Path) -> String {
    let mut hash = Sha256::new();
    for path in files {
        let label = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        hash.update(label.as_bytes());
        hash.update([0]);
        hash.update(
            fs::read(path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display())),
        );
        hash.update([0]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut digest, "{byte:02x}").expect("string formatting");
    }
    digest
}

fn main() {
    let crate_root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let repository_root = crate_root.join("../..");
    let runtime_root = repository_root.join("runtime/javascript");
    let python_runtime_root = repository_root.join("runtime/python");
    let ruby_runtime_root = repository_root.join("runtime/ruby");
    let mut files = Vec::new();
    collect_files(&crate_root.join("src"), &mut files);
    collect_files(&runtime_root, &mut files);
    collect_files(&python_runtime_root, &mut files);
    collect_files(&ruby_runtime_root, &mut files);
    files.extend([crate_root.join("build.rs"), crate_root.join("Cargo.toml")]);
    files.sort();
    files.dedup();

    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rustc-env=SUPERCOV_ENGINE_SOURCE_SHA256={}",
        digest_files(&files, &repository_root)
    );
    let javascript_frontend = [
        crate_root.join("src/js_instrumenter.rs"),
        crate_root.join("src/probe_v2.rs"),
        crate_root.join("Cargo.toml"),
    ];
    let mut javascript_frontend = javascript_frontend.to_vec();
    collect_files(&runtime_root, &mut javascript_frontend);
    javascript_frontend.sort();
    javascript_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_JS_FRONTEND_SOURCE_SHA256={}",
        digest_files(&javascript_frontend, &repository_root)
    );
    let mut python_frontend = vec![
        crate_root.join("src/python_instrumenter.rs"),
        crate_root.join("src/python_evidence.rs"),
        crate_root.join("src/python_project.rs"),
        crate_root.join("Cargo.toml"),
    ];
    collect_files(&python_runtime_root, &mut python_frontend);
    python_frontend.sort();
    python_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_PYTHON_FRONTEND_SOURCE_SHA256={}",
        digest_files(&python_frontend, &repository_root)
    );
    let mut ruby_frontend = vec![
        crate_root.join("src/ruby_instrumenter.rs"),
        crate_root.join("src/ruby_evidence.rs"),
        crate_root.join("src/ruby_project.rs"),
        crate_root.join("Cargo.toml"),
    ];
    collect_files(&ruby_runtime_root, &mut ruby_frontend);
    ruby_frontend.sort();
    ruby_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_RUBY_FRONTEND_SOURCE_SHA256={}",
        digest_files(&ruby_frontend, &repository_root)
    );
}
