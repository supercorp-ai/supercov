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

fn digest_files(files: &[PathBuf], crate_root: &Path) -> String {
    let mut hash = Sha256::new();
    for path in files {
        let label = path
            .strip_prefix(crate_root)
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

/// The Kotlin grammar Supercov builds itself: upstream's generated parser
/// with scanner fixes it has not released (see its README).
const KOTLIN_GRAMMAR: &str = "vendor/tree-sitter-kotlin";

fn compile_kotlin_grammar(crate_root: &Path) {
    let source = crate_root.join(KOTLIN_GRAMMAR).join("src");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(&source)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build
        .file(source.join("parser.c"))
        .file(source.join("scanner.c"))
        .compile("supercov-tree-sitter-kotlin");
}

/// The call sites `src` names for fault injection, written to
/// `fault_sites.rs` for `crate::fault` to include. A line holding only
/// `// fault-site: NAME` names the call on the line below it, so a test fails
/// that call by its name however the lines above it move. A name is
/// lowercase words joined by `-` and `.`, with a `.` at least. The build
/// fails on a name given twice, a name that is no such word, or a comment
/// with no code below it. (The CLI's build script holds the same function
/// for its own sites.)
fn name_fault_sites(crate_root: &Path) {
    let source = crate_root.join("src");
    let mut files = Vec::new();
    collect_files(&source, &mut files);
    let mut named = std::collections::BTreeMap::new();
    let mut table = String::from("&[\n");
    for path in files
        .iter()
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
    {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let relative = path
            .strip_prefix(&source)
            .expect("a source under src")
            .to_string_lossy()
            .replace('\\', "/");
        let lines = text.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            let Some(name) = line.trim().strip_prefix("// fault-site:") else {
                continue;
            };
            let name = name.trim();
            let place = format!("src/{relative}:{}", index + 1);
            let word = |word: &str| {
                !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            };
            assert!(
                name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name.contains('.')
                    && name.split(['-', '.']).all(word),
                "{place}: `{name}` is no fault site name: \
                 lowercase words joined by `-` and `.`, with a `.` at least"
            );
            let below = lines.get(index + 1).map_or("", |line| line.trim());
            assert!(
                !below.is_empty() && !below.starts_with("//"),
                "{place}: the fault site `{name}` names no call: the line below it is no code"
            );
            if let Some(earlier) = named.insert(name.to_owned(), place.clone()) {
                panic!("two fault sites are named `{name}`: {earlier} and {place}");
            }
            writeln!(table, "    ({name:?}, {relative:?}, {}),", index + 2)
                .expect("string formatting");
        }
    }
    table.push(']');
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("output directory"));
    fs::write(out.join("fault_sites.rs"), table).expect("failed to write fault_sites.rs");
}

fn main() {
    // Everything digested lives inside the crate: the runtime shims are
    // synchronised copies under runtime-assets/, so the tarball cargo packages
    // builds from source, and every label is relative to the crate rather than
    // to a repository checkout that a packaged build does not have.
    let crate_root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let runtime_root = crate_root.join("runtime-assets/javascript");
    let python_runtime_root = crate_root.join("runtime-assets/python");
    let ruby_runtime_root = crate_root.join("runtime-assets/ruby");
    let go_runtime_root = crate_root.join("runtime-assets/go");
    let jvm_runtime_root = crate_root.join("runtime-assets/jvm");
    compile_kotlin_grammar(&crate_root);
    name_fault_sites(&crate_root);
    let mut files = Vec::new();
    collect_files(&crate_root.join("src"), &mut files);
    collect_files(&runtime_root, &mut files);
    collect_files(&python_runtime_root, &mut files);
    collect_files(&ruby_runtime_root, &mut files);
    collect_files(&go_runtime_root, &mut files);
    collect_files(&jvm_runtime_root, &mut files);
    collect_files(&crate_root.join(KOTLIN_GRAMMAR), &mut files);
    files.extend([crate_root.join("build.rs"), crate_root.join("Cargo.toml")]);
    files.sort();
    files.dedup();

    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rustc-env=SUPERCOV_ENGINE_SOURCE_SHA256={}",
        digest_files(&files, &crate_root)
    );
    // What a run's query index is made of: the analysis of its stored
    // evidence and the index written from it. The evidence was normalised
    // when the run was measured, so instrumenters, runtimes, frontends,
    // queries and the CLI do not change it, and a release that changes only
    // them does not re-index every stored run on its first query.
    let analysis = [
        "coverage_report.rs",
        "coverage_analysis.rs",
        "coverage_index.rs",
        "query_index.rs",
        "frontend_protocol.rs",
        "evidence_archive.rs",
        "interned.rs",
        "test_directory.rs",
        "run_store.rs",
    ]
    .map(|file| crate_root.join("src").join(file));
    println!(
        "cargo:rustc-env=SUPERCOV_ANALYSIS_SOURCE_SHA256={}",
        digest_files(&analysis, &crate_root)
    );
    let javascript_frontend = [
        crate_root.join("src/js_instrumenter.rs"),
        crate_root.join("src/typescript_imports.rs"),
        crate_root.join("src/javascript_frontend.rs"),
        crate_root.join("src/probe_v2.rs"),
        crate_root.join("Cargo.toml"),
    ];
    let mut javascript_frontend = javascript_frontend.to_vec();
    collect_files(&runtime_root, &mut javascript_frontend);
    javascript_frontend.sort();
    javascript_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_JS_FRONTEND_SOURCE_SHA256={}",
        digest_files(&javascript_frontend, &crate_root)
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
        digest_files(&python_frontend, &crate_root)
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
        digest_files(&ruby_frontend, &crate_root)
    );
    let mut go_frontend = vec![
        crate_root.join("src/go_instrumenter.rs"),
        crate_root.join("src/go_project.rs"),
        crate_root.join("src/go_test_harness.rs"),
        crate_root.join("src/owned_evidence.rs"),
        crate_root.join("Cargo.toml"),
    ];
    collect_files(&go_runtime_root, &mut go_frontend);
    go_frontend.sort();
    go_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_GO_FRONTEND_SOURCE_SHA256={}",
        digest_files(&go_frontend, &crate_root)
    );
    let mut jvm_frontend = vec![
        crate_root.join("src/jvm_instrumenter.rs"),
        crate_root.join("src/jvm_project.rs"),
        crate_root.join("src/owned_evidence.rs"),
        crate_root.join("Cargo.toml"),
    ];
    collect_files(&jvm_runtime_root, &mut jvm_frontend);
    collect_files(&crate_root.join(KOTLIN_GRAMMAR), &mut jvm_frontend);
    jvm_frontend.sort();
    jvm_frontend.dedup();
    println!(
        "cargo:rustc-env=SUPERCOV_JVM_FRONTEND_SOURCE_SHA256={}",
        digest_files(&jvm_frontend, &crate_root)
    );
}
