//! What a run captures of its sources: each file's text at the time of the
//! run, fingerprinted into the manifest archived with the evidence, and the
//! check that a later query reads the same bytes.
use crate::fault_fs as fs;
use crate::{
    evidence_archive::EvidenceArchiveEntry,
    source_manifest::{FileFingerprint, Files, InputManifest, Inputs, local_path},
    workspace::{canonicalize_simplified, simplified},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The archive entry holding the run's source manifest.
pub const ARCHIVE_PATH: &str = "source-inputs.json";

/// The text of every given file under `root`, keyed project-relative.
pub fn capture(
    root: &Path,
    language: &str,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Inputs, String> {
    let supplied_root = simplified(root.to_owned());
    let root = canonicalize_simplified(root).map_err(|e| e.to_string())?;
    let mut inputs = Inputs {
        language: language.into(),
        files: Files::new(),
    };
    for path in paths.into_iter().map(simplified).collect::<BTreeSet<_>>() {
        let full = if path.is_absolute() {
            root.join(path.strip_prefix(&supplied_root).unwrap_or(&path))
        } else {
            root.join(&path)
        };
        if !full.exists() {
            continue;
        }
        let relative = full
            .strip_prefix(&root)
            .map_err(|_| format!("source input outside project: {}", full.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !local_path(&relative)
            || !canonicalize_simplified(&full)
                .map_err(|e| e.to_string())?
                .starts_with(&root)
        {
            return Err(format!("source input outside project: {relative}"));
        }
        if inputs.files.contains_key(&relative) {
            continue;
        }
        let bytes = fs::read(&full).map_err(|e| format!("{relative}: {e}"))?;
        // A file that is not UTF-8 has no text to fingerprint by declaration
        // or to show; it is left out.
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        inputs.files.insert(relative, text);
    }
    Ok(inputs)
}

pub fn append(
    mut entries: Vec<EvidenceArchiveEntry>,
    inputs: &Inputs,
) -> Result<Vec<EvidenceArchiveEntry>, String> {
    if entries.iter().any(|e| e.path == ARCHIVE_PATH) {
        return Err("duplicate source inputs".into());
    }
    entries.push(EvidenceArchiveEntry {
        path: ARCHIVE_PATH.into(),
        contents: serde_json::to_vec(&inputs.manifest()).expect("plain data serializes"),
    });
    Ok(entries)
}

/// Read project files only when their exact bytes still match the run manifest.
pub fn current_sources(root: &Path, manifest: &InputManifest) -> Result<Inputs, String> {
    current_sources_for(root, manifest, |_| true)
}

/// [`current_sources`] for only the files `needed` names: a file the run
/// recorded that the caller does not read -- a configuration file only CI
/// generates, say -- may be missing or changed.
pub fn current_sources_for(
    root: &Path,
    manifest: &InputManifest,
    needed: impl Fn(&str) -> bool,
) -> Result<Inputs, String> {
    let root = canonicalize_simplified(root).map_err(|e| e.to_string())?;
    let mut files = Files::new();
    for (file, expected) in manifest.files.iter().filter(|(file, _)| needed(file)) {
        if !local_path(file) {
            return Err(format!("Invalid source path: {file}"));
        }
        let path = root.join(file);
        let source = (|| {
            let canonical = canonicalize_simplified(&path).ok()?;
            if !canonical.starts_with(&root) || !canonical.is_file() {
                return None;
            }
            let text = fs::read_to_string(canonical).ok()?;
            FileFingerprint::of(&text)
                .same_bytes(expected)
                .then_some(text)
        })();
        let Some(source) = source else {
            return Err(format!(
                "Current source differs from the run or is unavailable: {file}; rerun tests for the current checkout"
            ));
        };
        files.insert(file.clone(), source);
    }
    Ok(manifest.with_sources(files))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_directory::TestDirectory;
    use std::os::unix::fs::symlink;

    /// A project with one source file, and a file outside it that a link in
    /// the project names.
    fn project(label: &str) -> (TestDirectory, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "supercov-capture-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.js"), "const a = 1;\n").unwrap();
        fs::write(base.join("secret.js"), "const secret = 1;\n").unwrap();
        symlink(base.join("secret.js"), root.join("src/linked.js")).unwrap();
        (TestDirectory::adopt(base), root)
    }

    #[test]
    fn a_source_reached_by_a_climb_or_a_link_out_of_the_project_is_refused() {
        let (_base, root) = project("outside");
        let captured = |path: &str| capture(&root, "javascript", [PathBuf::from(path)]);
        assert_eq!(
            captured("src/a.js").unwrap().files["src/a.js"],
            "const a = 1;\n"
        );
        assert_eq!(
            captured("src/../src/a.js").unwrap_err(),
            "source input outside project: src/../src/a.js"
        );
        assert_eq!(
            captured("src/linked.js").unwrap_err(),
            "source input outside project: src/linked.js"
        );
    }

    /// Nothing named is nothing captured; a file that is not UTF-8 or no
    /// longer there is left out; a root that is gone is an error, and so is
    /// reading the run's files back under it.
    #[test]
    fn what_cannot_be_captured_is_left_out_or_refused() {
        let (_base, root) = project("left-out");
        assert!(
            capture(&root, "javascript", Vec::<PathBuf>::new())
                .unwrap()
                .files
                .is_empty()
        );
        fs::write(root.join("src/binary.js"), [0xff, 0xfe]).unwrap();
        let captured = capture(
            &root,
            "javascript",
            ["src/a.js", "src/binary.js", "src/gone.js"].map(PathBuf::from),
        )
        .unwrap();
        assert_eq!(captured.files.keys().collect::<Vec<_>>(), ["src/a.js"]);
        let gone = root.join("gone");
        assert!(capture(&gone, "javascript", [PathBuf::from("src/a.js")]).is_err());
        assert!(current_sources(&gone, &captured.manifest()).is_err());
    }

    #[test]
    fn source_inputs_are_archived_once() {
        let (_base, root) = project("archived");
        let inputs = capture(&root, "javascript", [PathBuf::from("src/a.js")]).unwrap();
        let entries = append(Vec::new(), &inputs).unwrap();
        assert_eq!(entries[0].path, ARCHIVE_PATH);
        assert_eq!(
            append(entries, &inputs).unwrap_err(),
            "duplicate source inputs"
        );
    }

    #[test]
    fn a_recorded_source_is_read_back_only_from_a_file_in_the_project() {
        let (_base, root) = project("current");
        let mut manifest = capture(&root, "javascript", [PathBuf::from("src/a.js")])
            .unwrap()
            .manifest();
        assert_eq!(
            current_sources(&root, &manifest).unwrap().files["src/a.js"],
            "const a = 1;\n"
        );
        let read = |manifest: &InputManifest| current_sources(&root, manifest).unwrap_err();
        let fingerprint = manifest.files["src/a.js"].clone();
        let mut climbing = manifest.clone();
        climbing
            .files
            .insert("../secret.js".into(), fingerprint.clone());
        assert_eq!(read(&climbing), "Invalid source path: ../secret.js");
        let unavailable = |file: &str| {
            format!(
                "Current source differs from the run or is unavailable: {file}; rerun tests for the current checkout"
            )
        };
        manifest.files.insert(
            "src/linked.js".into(),
            FileFingerprint::of("const secret = 1;\n"),
        );
        assert_eq!(read(&manifest), unavailable("src/linked.js"));
        manifest.files.remove("src/linked.js");
        manifest.files.insert("src".into(), fingerprint);
        assert_eq!(read(&manifest), unavailable("src"));
    }
}
