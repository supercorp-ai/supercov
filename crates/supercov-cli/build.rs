//! The CLI's fault sites, named as the engine names its own (see the
//! engine's build script): `main` hands the table to `supercov_engine::fault`.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

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

/// The call sites `src` names for fault injection, written to
/// `fault_sites.rs` for `main` to hand to
/// `supercov_engine::fault`. A line holding only
/// `// fault-site: NAME` names the call on the line below it, so a test fails
/// that call by its name however the lines above it move. A name is
/// lowercase words joined by `-` and `.`, with a `.` at least. The build
/// fails on a name given twice, a name that is no such word, or a comment
/// with no code below it. (The engine's build script holds the same function
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
    let crate_root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    name_fault_sites(&crate_root);
}
