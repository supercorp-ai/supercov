//! What a run captures of its sources: each file's text at the time of the
//! run, fingerprinted into the manifest archived with the evidence, and the
//! check that a later query reads the same bytes.
use crate::{
    evidence_archive::EvidenceArchiveEntry,
    source_manifest::{FileFingerprint, Files, InputManifest, Inputs, local_path},
    workspace::{canonicalize_simplified, simplified},
};
use std::{
    collections::BTreeSet,
    fs,
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
        contents: serde_json::to_vec(&inputs.manifest()).map_err(|e| e.to_string())?,
    });
    Ok(entries)
}

/// Read project files only when their exact bytes still match the run manifest.
pub fn current_sources(root: &Path, manifest: &InputManifest) -> Result<Inputs, String> {
    let root = canonicalize_simplified(root).map_err(|e| e.to_string())?;
    let mut files = Files::new();
    for (file, expected) in &manifest.files {
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
