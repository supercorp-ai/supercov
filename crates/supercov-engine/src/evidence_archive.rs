//! Language-neutral, streaming implementation of the frozen evidence archive.
//!
//! The framing contract is the authority. In particular, the reader does not
//! preserve historical permissiveness from the JavaScript implementation:
//! paths must be canonical and sorted, headers must use the exact canonical
//! JSON encoding, and a gzip member may not hide trailing data.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use flate2::{Compression, GzBuilder, bufread::GzDecoder};
use serde::{Deserialize, Serialize};
use supercov_contracts::{EVIDENCE_ARCHIVE_MAGIC, EVIDENCE_ARCHIVE_SCHEMA_VERSION};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum EvidenceArchiveError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidMagic,
    InvalidHeader(&'static str),
    InvalidPath(String),
    DuplicatePath(String),
    UnsortedPath { previous: String, current: String },
    MissingManifest,
    Truncated(&'static str),
    TrailingCompressedData,
    UnsupportedSource(PathBuf),
    PathOutsideSource { source: PathBuf, path: PathBuf },
    SizeOverflow,
}

impl std::fmt::Display for EvidenceArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::InvalidMagic => write!(formatter, "unsupported Supercov evidence archive"),
            Self::InvalidHeader(reason) => {
                write!(
                    formatter,
                    "invalid Supercov evidence archive header: {reason}"
                )
            }
            Self::InvalidPath(path) => {
                write!(
                    formatter,
                    "invalid Supercov evidence archive path: {path:?}"
                )
            }
            Self::DuplicatePath(path) => {
                write!(
                    formatter,
                    "duplicate Supercov evidence archive path: {path}"
                )
            }
            Self::UnsortedPath { previous, current } => write!(
                formatter,
                "unsorted Supercov evidence archive paths: {previous} before {current}",
            ),
            Self::MissingManifest => write!(
                formatter,
                "Supercov evidence archive is missing manifest.json",
            ),
            Self::Truncated(part) => {
                write!(formatter, "truncated Supercov evidence archive {part}")
            }
            Self::TrailingCompressedData => write!(
                formatter,
                "Supercov evidence archive contains trailing compressed data",
            ),
            Self::UnsupportedSource(path) => {
                write!(
                    formatter,
                    "unsupported raw evidence entry: {}",
                    path.display()
                )
            }
            Self::PathOutsideSource { source, path } => write!(
                formatter,
                "evidence path {} is outside source {}",
                path.display(),
                source.display(),
            ),
            Self::SizeOverflow => write!(formatter, "evidence archive size exceeds its format"),
        }
    }
}

impl std::error::Error for EvidenceArchiveError {}

impl From<io::Error> for EvidenceArchiveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for EvidenceArchiveError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceArchiveEntry {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceArchiveMetadata {
    pub schema_version: u32,
    pub format: &'static str,
    pub file: &'static str,
    pub files: usize,
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceArchiveSource {
    Directory {
        directory: PathBuf,
        prefix: Option<String>,
    },
    File {
        file: PathBuf,
        path: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntryHeader {
    path: String,
    bytes: u64,
}

struct CountingWriter<W> {
    inner: W,
    written: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, written: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.written = self
            .written
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("evidence archive size overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn validate_archive_path(path: &str) -> Result<(), EvidenceArchiveError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(EvidenceArchiveError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn validate_prefix(prefix: &str) -> Result<(), EvidenceArchiveError> {
    validate_archive_path(prefix)
}

fn path_from_relative(path: &Path) -> Result<String, EvidenceArchiveError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| EvidenceArchiveError::InvalidPath(path.display().to_string()))?;
                parts.push(part);
            }
            _ => {
                return Err(EvidenceArchiveError::InvalidPath(
                    path.display().to_string(),
                ));
            }
        }
    }
    let archive_path = parts.join("/");
    validate_archive_path(&archive_path)?;
    Ok(archive_path)
}

fn collect_directory(
    root: &Path,
    current: &Path,
    prefix: Option<&str>,
    entries: &mut Vec<EvidenceArchiveEntry>,
) -> Result<(), EvidenceArchiveError> {
    let mut children = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_directory(root, &path, prefix, entries)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(EvidenceArchiveError::UnsupportedSource(path));
        }
        let relative =
            path.strip_prefix(root)
                .map_err(|_| EvidenceArchiveError::PathOutsideSource {
                    source: root.to_owned(),
                    path: path.clone(),
                })?;
        let relative = path_from_relative(relative)?;
        let archive_path = match prefix {
            Some(prefix) => format!("{prefix}/{relative}"),
            None => relative,
        };
        validate_archive_path(&archive_path)?;
        entries.push(EvidenceArchiveEntry {
            path: archive_path,
            contents: fs::read(path)?,
        });
    }
    Ok(())
}

pub fn collect_sources(
    sources: &[EvidenceArchiveSource],
) -> Result<Vec<EvidenceArchiveEntry>, EvidenceArchiveError> {
    let mut entries = Vec::new();
    for source in sources {
        match source {
            EvidenceArchiveSource::Directory { directory, prefix } => {
                if let Some(prefix) = prefix {
                    validate_prefix(prefix)?;
                }
                match fs::symlink_metadata(directory) {
                    Ok(metadata) if metadata.is_dir() => {
                        collect_directory(directory, directory, prefix.as_deref(), &mut entries)?
                    }
                    Ok(_) => {
                        return Err(EvidenceArchiveError::UnsupportedSource(directory.clone()));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            EvidenceArchiveSource::File { file, path } => {
                validate_archive_path(path)?;
                let metadata = fs::symlink_metadata(file)?;
                if !metadata.is_file() {
                    return Err(EvidenceArchiveError::UnsupportedSource(file.clone()));
                }
                entries.push(EvidenceArchiveEntry {
                    path: path.clone(),
                    contents: fs::read(file)?,
                });
            }
        }
    }
    canonicalize_entries(entries)
}

fn canonicalize_entries(
    mut entries: Vec<EvidenceArchiveEntry>,
) -> Result<Vec<EvidenceArchiveEntry>, EvidenceArchiveError> {
    for entry in &entries {
        validate_archive_path(&entry.path)?;
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    for pair in entries.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(EvidenceArchiveError::DuplicatePath(pair[0].path.clone()));
        }
    }
    if !entries.iter().any(|entry| entry.path == "manifest.json") {
        return Err(EvidenceArchiveError::MissingManifest);
    }
    Ok(entries)
}

fn write_framed<W: Write>(
    entries: &[EvidenceArchiveEntry],
    output: W,
) -> Result<(W, u64), EvidenceArchiveError> {
    let mut writer = CountingWriter::new(output);
    writer.write_all(EVIDENCE_ARCHIVE_MAGIC.as_bytes())?;
    for entry in entries {
        let header = EntryHeader {
            path: entry.path.clone(),
            bytes: u64::try_from(entry.contents.len())
                .map_err(|_| EvidenceArchiveError::SizeOverflow)?,
        };
        let mut encoded = serde_json::to_vec(&header)?;
        encoded.push(b'\n');
        let header_size =
            u32::try_from(encoded.len()).map_err(|_| EvidenceArchiveError::SizeOverflow)?;
        writer.write_all(&header_size.to_be_bytes())?;
        writer.write_all(&encoded)?;
        writer.write_all(&entry.contents)?;
    }
    writer.flush()?;
    Ok((writer.inner, writer.written))
}

fn temporary_path(destination: &Path, sequence: u64) -> Result<PathBuf, EvidenceArchiveError> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| EvidenceArchiveError::InvalidPath(destination.display().to_string()))?;
    Ok(destination.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence,)))
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

pub fn write_archive(
    entries: Vec<EvidenceArchiveEntry>,
    destination: &Path,
) -> Result<EvidenceArchiveMetadata, EvidenceArchiveError> {
    let entries = canonicalize_entries(entries)?;
    let parent = destination
        .parent()
        .ok_or_else(|| EvidenceArchiveError::InvalidPath(destination.display().to_string()))?;
    fs::create_dir_all(parent)?;

    let (temporary, file) = loop {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = temporary_path(destination, sequence)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };

    let result = (|| {
        let buffered = BufWriter::new(file);
        let gzip = GzBuilder::new()
            .mtime(0)
            .operating_system(255)
            .write(buffered, Compression::best());
        let (gzip, uncompressed_bytes) = write_framed(&entries, gzip)?;
        let buffered = gzip.finish()?;
        let file = buffered
            .into_inner()
            .map_err(|error| EvidenceArchiveError::Io(error.into_error()))?;
        file.sync_all()?;
        let compressed_bytes = file.metadata()?.len();
        drop(file);
        fs::rename(&temporary, destination)?;
        sync_parent(destination);
        Ok(EvidenceArchiveMetadata {
            schema_version: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
            format: "framed+gzip",
            file: "evidence.raw.gz",
            files: entries.len(),
            uncompressed_bytes,
            compressed_bytes,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_exact_or_truncated<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    part: &'static str,
) -> Result<(), EvidenceArchiveError> {
    reader
        .read_exact(buffer)
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => EvidenceArchiveError::Truncated(part),
            _ => EvidenceArchiveError::Io(error),
        })
}

fn read_framed<R: Read>(reader: &mut R) -> Result<Vec<EvidenceArchiveEntry>, EvidenceArchiveError> {
    let mut magic = vec![0; EVIDENCE_ARCHIVE_MAGIC.len()];
    read_exact_or_truncated(reader, &mut magic, "magic")?;
    if magic != EVIDENCE_ARCHIVE_MAGIC.as_bytes() {
        return Err(EvidenceArchiveError::InvalidMagic);
    }
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous: Option<String> = None;
    loop {
        let mut encoded_size = [0; 4];
        let first = reader.read(&mut encoded_size[..1])?;
        if first == 0 {
            break;
        }
        read_exact_or_truncated(reader, &mut encoded_size[1..], "header length")?;
        let header_size = u32::from_be_bytes(encoded_size) as usize;
        if header_size == 0 {
            return Err(EvidenceArchiveError::InvalidHeader("empty header"));
        }
        let mut encoded_header = vec![0; header_size];
        read_exact_or_truncated(reader, &mut encoded_header, "header")?;
        if encoded_header.last() != Some(&b'\n') {
            return Err(EvidenceArchiveError::InvalidHeader(
                "header is not newline terminated",
            ));
        }
        let header: EntryHeader =
            serde_json::from_slice(&encoded_header[..encoded_header.len() - 1])?;
        let mut canonical = serde_json::to_vec(&header)?;
        canonical.push(b'\n');
        if encoded_header != canonical {
            return Err(EvidenceArchiveError::InvalidHeader(
                "header is not canonical JSON",
            ));
        }
        validate_archive_path(&header.path)?;
        if let Some(previous) = &previous {
            if previous == &header.path {
                return Err(EvidenceArchiveError::DuplicatePath(header.path));
            }
            if previous > &header.path {
                return Err(EvidenceArchiveError::UnsortedPath {
                    previous: previous.clone(),
                    current: header.path,
                });
            }
        }
        if !seen.insert(header.path.clone()) {
            return Err(EvidenceArchiveError::DuplicatePath(header.path));
        }
        let payload_size =
            usize::try_from(header.bytes).map_err(|_| EvidenceArchiveError::SizeOverflow)?;
        let mut contents = vec![0; payload_size];
        read_exact_or_truncated(reader, &mut contents, "payload")?;
        previous = Some(header.path.clone());
        entries.push(EvidenceArchiveEntry {
            path: header.path,
            contents,
        });
    }
    if !seen.contains("manifest.json") {
        return Err(EvidenceArchiveError::MissingManifest);
    }
    Ok(entries)
}

pub fn read_archive(path: &Path) -> Result<Vec<EvidenceArchiveEntry>, EvidenceArchiveError> {
    let input = BufReader::new(File::open(path)?);
    let mut decoder = GzDecoder::new(input);
    let entries = read_framed(&mut decoder)?;
    // bufread::GzDecoder deliberately stops at the end of one member without
    // consuming following bytes, which lets us reject concatenated members or
    // arbitrary trailing compressed data instead of silently trusting them.
    let mut input = decoder.into_inner();
    let mut trailing = [0; 1];
    if input.read(&mut trailing)? != 0 {
        return Err(EvidenceArchiveError::TrailingCompressedData);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };

    use flate2::{Compression, GzBuilder, write::GzEncoder};

    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-rust-{label}-{}-{nonce}",
            std::process::id(),
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn entry(path: &str, contents: &[u8]) -> EvidenceArchiveEntry {
        EvidenceArchiveEntry {
            path: path.to_owned(),
            contents: contents.to_vec(),
        }
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn frame(entries: &[EvidenceArchiveEntry]) -> Vec<u8> {
        let (bytes, _) = write_framed(entries, Vec::new()).unwrap();
        bytes
    }

    fn write_compressed(root: &Path, bytes: &[u8]) -> PathBuf {
        let path = root.join("archive.gz");
        fs::write(&path, gzip(bytes)).unwrap();
        path
    }

    #[test]
    fn round_trips_binary_evidence_in_canonical_unicode_order() {
        let root = temporary_directory("archive-roundtrip");
        let first = root.join("first.gz");
        let second = root.join("second.gz");
        let entries = vec![
            entry("𐀀/result.bin", &[0, 255, b'\n']),
            entry("manifest.json", br#"{"decisions":[]}"#),
            entry("é/result.jsonl", b"{}\n"),
            entry("\u{e000}/result.jsonl", b"private\n"),
            entry("a/result.jsonl", b"{\"hit\":true}\n"),
        ];
        let metadata = write_archive(entries.clone(), &first).unwrap();
        write_archive(entries, &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        assert_eq!(metadata.schema_version, EVIDENCE_ARCHIVE_SCHEMA_VERSION);
        assert_eq!(metadata.files, 5);
        assert!(metadata.uncompressed_bytes > 0);
        assert!(metadata.compressed_bytes > 0);
        assert_eq!(
            read_archive(&first).unwrap(),
            vec![
                entry("a/result.jsonl", b"{\"hit\":true}\n"),
                entry("manifest.json", br#"{"decisions":[]}"#),
                entry("é/result.jsonl", b"{}\n"),
                entry("\u{e000}/result.jsonl", b"private\n"),
                entry("𐀀/result.bin", &[0, 255, b'\n']),
            ],
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_every_noncanonical_framing_boundary() {
        let root = temporary_directory("archive-invalid");
        let manifest = entry("manifest.json", b"{}");
        let valid = frame(std::slice::from_ref(&manifest));
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "invalid magic",
                [b"WRONG\n".as_slice(), &valid[6..]].concat(),
            ),
            (
                "truncated header length",
                [valid.as_slice(), &[0, 0]].concat(),
            ),
            (
                "truncated header",
                [EVIDENCE_ARCHIVE_MAGIC.as_bytes(), &[0, 0, 0, 9], b"{}\n"].concat(),
            ),
            (
                "noncanonical header",
                [
                    EVIDENCE_ARCHIVE_MAGIC.as_bytes(),
                    &(36_u32.to_be_bytes()),
                    b"{ \"path\":\"manifest.json\",\"bytes\":0}\n",
                ]
                .concat(),
            ),
            (
                "truncated payload",
                [
                    EVIDENCE_ARCHIVE_MAGIC.as_bytes(),
                    &(35_u32.to_be_bytes()),
                    b"{\"path\":\"manifest.json\",\"bytes\":2}\n",
                    b"{",
                ]
                .concat(),
            ),
            (
                "trailing decompressed data",
                [valid.as_slice(), b"x"].concat(),
            ),
        ];
        for (label, bytes) in cases {
            let path = write_compressed(&root, &bytes);
            assert!(read_archive(&path).is_err(), "{label}");
        }

        let duplicate = frame(&[manifest.clone(), manifest.clone()]);
        assert!(read_archive(&write_compressed(&root, &duplicate)).is_err());
        let unsorted = frame(&[manifest.clone(), entry("a", b"")]);
        assert!(read_archive(&write_compressed(&root, &unsorted)).is_err());
        let missing = frame(&[entry("result.json", b"{}")]);
        assert!(read_archive(&write_compressed(&root, &missing)).is_err());

        let path = root.join("trailing-compressed.gz");
        let mut compressed = gzip(&valid);
        compressed.push(0);
        fs::write(&path, compressed).unwrap();
        assert!(matches!(
            read_archive(&path),
            Err(EvidenceArchiveError::TrailingCompressedData)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_duplicate_and_missing_manifest_entries_without_debris() {
        let root = temporary_directory("archive-safety");
        let destination = root.join("evidence.raw.gz");
        for invalid in ["", "/absolute", "../escape", "a/../escape", "a\\b"] {
            assert!(write_archive(vec![entry(invalid, b"")], &destination).is_err());
        }
        assert!(
            write_archive(
                vec![entry("manifest.json", b"{}"), entry("manifest.json", b"{}")],
                &destination,
            )
            .is_err(),
        );
        assert!(write_archive(vec![entry("result.json", b"{}")], &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collects_only_regular_files_and_normalizes_host_separators() {
        let root = temporary_directory("archive-sources");
        let evidence = root.join("evidence");
        fs::create_dir_all(evidence.join("worker")).unwrap();
        fs::write(evidence.join("worker/result.jsonl"), b"{}\n").unwrap();
        let manifest = root.join("manifest.json");
        fs::write(&manifest, b"{}\n").unwrap();
        let entries = collect_sources(&[
            EvidenceArchiveSource::Directory {
                directory: evidence,
                prefix: Some("server".to_owned()),
            },
            EvidenceArchiveSource::File {
                file: manifest,
                path: "manifest.json".to_owned(),
            },
        ])
        .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["manifest.json", "server/worker/result.jsonl"],
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_sources_instead_of_following_them() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory("archive-links");
        let evidence = root.join("evidence");
        fs::create_dir_all(&evidence).unwrap();
        fs::write(root.join("outside"), b"secret").unwrap();
        symlink(root.join("outside"), evidence.join("linked")).unwrap();
        assert!(matches!(
            collect_sources(&[EvidenceArchiveSource::Directory {
                directory: evidence,
                prefix: None,
            }]),
            Err(EvidenceArchiveError::UnsupportedSource(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deterministic_gzip_header_has_no_clock_or_host_identity() {
        let mut first = Vec::new();
        let mut second = Vec::new();
        for destination in [&mut first, &mut second] {
            let encoder = GzBuilder::new()
                .mtime(0)
                .operating_system(255)
                .write(destination, Compression::best());
            let (encoder, _) = write_framed(&[entry("manifest.json", b"{}")], encoder).unwrap();
            encoder.finish().unwrap();
        }
        assert_eq!(first, second);
        assert_eq!(&first[4..8], &[0, 0, 0, 0]);
        assert_eq!(first[9], 255);
    }
}
