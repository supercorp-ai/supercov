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
        } else if entry.extension().is_some_and(|extension| extension == "rs") {
            files.push(entry);
        }
    }
}

fn main() {
    let crate_root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let workspace_root = crate_root.join("../..");
    let contracts_root = crate_root.join("../supercov-contracts");
    let mut files = Vec::new();
    collect_files(&crate_root.join("src"), &mut files);
    collect_files(&contracts_root.join("src"), &mut files);
    files.extend([
        crate_root.join("build.rs"),
        crate_root.join("Cargo.toml"),
        contracts_root.join("Cargo.toml"),
        workspace_root.join("Cargo.lock"),
    ]);
    files.sort();
    files.dedup();

    let mut hash = Sha256::new();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let label = path
            .strip_prefix(&workspace_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        hash.update(label.as_bytes());
        hash.update([0]);
        hash.update(
            fs::read(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display())),
        );
        hash.update([0]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut digest, "{byte:02x}").expect("string formatting");
    }
    println!("cargo:rustc-env=SUPERCOV_ENGINE_SOURCE_SHA256={digest}");
}
