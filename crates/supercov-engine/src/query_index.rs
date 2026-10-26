//! Immutable, versioned, fixed-layout query-index container.
//!
//! The evidence archive remains authoritative. This index is disposable and
//! bound to the exact evidence, analysis inputs and producer ABI. Sections are
//! opaque to this container; higher layers define their typed record layouts.
//! Reads validate the authenticated header, all checked layout invariants and
//! the SHA-256 digest of every data page they touch.

use crate::fault_fs::{self as fs, OpenOptions};
use std::{
    collections::{BTreeSet, HashSet},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use memmap2::Mmap;
use sha2::{Digest, Sha256};

pub const QUERY_INDEX_SCHEMA_VERSION: u32 = 5;
pub const QUERY_INDEX_HEADER_SIZE: usize = 4_096;
pub const QUERY_INDEX_PAGE_SIZE: usize = 64 * 1_024;
pub const QUERY_INDEX_MAX_SECTIONS: usize = 48;
const MAGIC: &[u8; 8] = b"SCQIDX01";
const HEADER_CHECKSUM_OFFSET: usize = QUERY_INDEX_HEADER_SIZE - 32;
const DESCRIPTOR_OFFSET: usize = 256;
const DESCRIPTOR_SIZE: usize = 64;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIndexIdentity {
    pub evidence_sha256: [u8; 32],
    pub evidence_bytes: u64,
    pub analysis_sha256: [u8; 32],
    pub producer_sha256: [u8; 32],
    pub archive_schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIndexSection {
    pub kind: u32,
    pub record_size: u32,
    pub count: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionDescriptor {
    pub kind: u32,
    pub record_size: u32,
    pub offset: u64,
    pub length: u64,
    pub count: u64,
    pub digests_offset: u64,
    pub digest_count: u32,
}

#[derive(Debug)]
pub enum QueryIndexError {
    Io(io::Error),
    NotRegularFile(PathBuf),
    InvalidHeader(&'static str),
    IdentityMismatch(&'static str),
    TooManySections(usize),
    InvalidSection { kind: u32, reason: &'static str },
    DuplicateSection(u32),
    MissingSection(u32),
    OutOfBounds { kind: u32, offset: u64, length: u64 },
    CorruptPage { kind: u32, page: usize },
    SizeOverflow,
}

impl std::fmt::Display for QueryIndexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NotRegularFile(path) => write!(
                formatter,
                "query index is not a regular file: {}",
                path.display()
            ),
            Self::InvalidHeader(reason) => {
                write!(formatter, "invalid query-index header: {reason}")
            }
            Self::IdentityMismatch(field) => write!(formatter, "stale query-index {field}"),
            Self::TooManySections(count) => write!(formatter, "query index has {count} sections"),
            Self::InvalidSection { kind, reason } => {
                write!(formatter, "invalid query-index section {kind}: {reason}")
            }
            Self::DuplicateSection(kind) => {
                write!(formatter, "duplicate query-index section {kind}")
            }
            Self::MissingSection(kind) => write!(formatter, "missing query-index section {kind}"),
            Self::OutOfBounds {
                kind,
                offset,
                length,
            } => write!(
                formatter,
                "query-index section {kind} range {offset}+{length} is out of bounds"
            ),
            Self::CorruptPage { kind, page } => {
                write!(
                    formatter,
                    "query-index section {kind} page {page} is corrupt"
                )
            }
            Self::SizeOverflow => write!(formatter, "query index exceeds its format limits"),
        }
    }
}

impl std::error::Error for QueryIndexError {}

impl From<io::Error> for QueryIndexError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn get<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], QueryIndexError> {
    bytes
        .get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(QueryIndexError::InvalidHeader("truncated field"))
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, QueryIndexError> {
    Ok(u32::from_le_bytes(get(bytes, offset)?))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, QueryIndexError> {
    Ok(u64::from_le_bytes(get(bytes, offset)?))
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn checked_end(offset: u64, length: u64) -> Result<u64, QueryIndexError> {
    offset
        .checked_add(length)
        .ok_or(QueryIndexError::SizeOverflow)
}

fn align(value: u64, alignment: u64) -> Result<u64, QueryIndexError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
        .ok_or(QueryIndexError::SizeOverflow)
}

fn temporary_path(destination: &Path, sequence: u64) -> Result<PathBuf, QueryIndexError> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(QueryIndexError::InvalidHeader("invalid destination name"))?;
    Ok(destination.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence)))
}

fn layout_sections(
    sections: &[QueryIndexSection],
) -> Result<(Vec<SectionDescriptor>, u64), QueryIndexError> {
    if sections.len() > QUERY_INDEX_MAX_SECTIONS {
        return Err(QueryIndexError::TooManySections(sections.len()));
    }
    let mut seen = HashSet::new();
    let mut ordered = sections.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|section| section.kind);
    for section in &ordered {
        if section.kind == 0 {
            return Err(QueryIndexError::InvalidSection {
                kind: 0,
                reason: "kind zero is reserved",
            });
        }
        if !seen.insert(section.kind) {
            return Err(QueryIndexError::DuplicateSection(section.kind));
        }
        if section.record_size > 0
            && u64::from(section.record_size)
                .checked_mul(section.count)
                .ok_or(QueryIndexError::SizeOverflow)?
                != section.bytes.len() as u64
        {
            return Err(QueryIndexError::InvalidSection {
                kind: section.kind,
                reason: "record size times count does not equal section length",
            });
        }
    }
    let mut cursor = QUERY_INDEX_HEADER_SIZE as u64;
    let mut descriptors = Vec::with_capacity(ordered.len());
    for section in &ordered {
        cursor = align(cursor, 8)?;
        // Lossless: `usize` is 64 bits on every target Supercov builds for.
        let length = section.bytes.len() as u64;
        let offset = cursor;
        cursor = checked_end(cursor, length)?;
        descriptors.push(SectionDescriptor {
            kind: section.kind,
            record_size: section.record_size,
            offset,
            length,
            count: section.count,
            digests_offset: 0,
            digest_count: u32::try_from(section.bytes.len().div_ceil(QUERY_INDEX_PAGE_SIZE))
                .expect("a section holds fewer than 2^32 pages: it is written from memory"),
        });
    }
    for descriptor in &mut descriptors {
        cursor = align(cursor, 8)?;
        descriptor.digests_offset = cursor;
        cursor = checked_end(cursor, u64::from(descriptor.digest_count) * 32)?;
    }
    Ok((descriptors, cursor))
}

fn make_header(
    identity: &QueryIndexIdentity,
    descriptors: &[SectionDescriptor],
    total_bytes: u64,
) -> Vec<u8> {
    let mut header = vec![0_u8; QUERY_INDEX_HEADER_SIZE];
    header[..8].copy_from_slice(MAGIC);
    put_u32(&mut header, 8, QUERY_INDEX_SCHEMA_VERSION);
    put_u32(&mut header, 12, QUERY_INDEX_HEADER_SIZE as u32);
    put_u64(&mut header, 16, total_bytes);
    put_u64(&mut header, 24, identity.evidence_bytes);
    put_u32(&mut header, 32, identity.archive_schema_version);
    put_u32(&mut header, 36, descriptors.len() as u32);
    put_u32(&mut header, 40, QUERY_INDEX_PAGE_SIZE as u32);
    header[48..80].copy_from_slice(&identity.evidence_sha256);
    header[80..112].copy_from_slice(&identity.analysis_sha256);
    header[112..144].copy_from_slice(&identity.producer_sha256);
    for (index, descriptor) in descriptors.iter().enumerate() {
        let offset = DESCRIPTOR_OFFSET + index * DESCRIPTOR_SIZE;
        put_u32(&mut header, offset, descriptor.kind);
        put_u32(&mut header, offset + 4, descriptor.record_size);
        put_u64(&mut header, offset + 8, descriptor.offset);
        put_u64(&mut header, offset + 16, descriptor.length);
        put_u64(&mut header, offset + 24, descriptor.count);
        put_u64(&mut header, offset + 32, descriptor.digests_offset);
        put_u32(&mut header, offset + 40, descriptor.digest_count);
    }
    let checksum = Sha256::digest(&header[..HEADER_CHECKSUM_OFFSET]);
    header[HEADER_CHECKSUM_OFFSET..].copy_from_slice(&checksum);
    header
}

pub fn write_query_index(
    sections: &[QueryIndexSection],
    identity: &QueryIndexIdentity,
    destination: &Path,
) -> Result<(), QueryIndexError> {
    let (descriptors, total_bytes) = layout_sections(sections)?;
    let parent = destination
        .parent()
        .ok_or(QueryIndexError::InvalidHeader("destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = loop {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = temporary_path(destination, sequence)?;
        match fs::open_with(OpenOptions::new().write(true).create_new(true), &temporary) {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| {
        file.set_len(total_bytes)?;
        let by_kind = sections
            .iter()
            .map(|section| (section.kind, section))
            .collect::<std::collections::HashMap<_, _>>();
        for descriptor in &descriptors {
            let section = by_kind[&descriptor.kind];
            file.seek(SeekFrom::Start(descriptor.offset))?;
            file.write_all(&section.bytes)?;
            file.seek(SeekFrom::Start(descriptor.digests_offset))?;
            for page in section.bytes.chunks(QUERY_INDEX_PAGE_SIZE) {
                file.write_all(&Sha256::digest(page))?;
            }
        }
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&make_header(identity, &descriptors, total_bytes))?;
        // Not synced: an index is disposable, and a reader verifies every
        // page against its digest, so one a machine crash left torn is
        // rebuilt from the evidence like a missing one. Syncing 90 MB cost
        // each publication of a 3,900-test run 50 ms.
        drop(file);
        fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The evidence an index says it was built from, its digest and length, read
/// from its header alone: what to expect when nothing has changed since the
/// index was verified (`run_store`). Opening the index checks the header,
/// its checksum included, so a damaged one is refused there.
pub fn recorded_evidence(path: &Path) -> Result<([u8; 32], u64), QueryIndexError> {
    let mut header = [0_u8; QUERY_INDEX_HEADER_SIZE];
    fs::open(path)?.read_exact(&mut header)?;
    let sha256 = header[48..80].try_into().expect("a 32-byte slice");
    Ok((sha256, get_u64(&header, 24)?))
}

pub struct QueryIndex {
    mmap: Mmap,
    descriptors: Vec<SectionDescriptor>,
    verified_pages: Mutex<BTreeSet<(u32, usize)>>,
}

impl QueryIndex {
    pub fn open(path: &Path, expected: &QueryIndexIdentity) -> Result<Self, QueryIndexError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(QueryIndexError::NotRegularFile(path.to_owned()));
        }
        let file = fs::open(path)?;
        // Supercov publishes a new immutable inode by rename and never mutates
        // the inode while it may be mapped. Readers retain this file handle.
        let mmap = unsafe { Mmap::map(&file)? };
        let header = mmap
            .get(..QUERY_INDEX_HEADER_SIZE)
            .ok_or(QueryIndexError::InvalidHeader("truncated"))?;
        if header.get(..8) != Some(MAGIC.as_slice()) {
            return Err(QueryIndexError::InvalidHeader("magic"));
        }
        if get_u32(header, 8)? != QUERY_INDEX_SCHEMA_VERSION {
            return Err(QueryIndexError::InvalidHeader("schema version"));
        }
        if get_u32(header, 12)? as usize != QUERY_INDEX_HEADER_SIZE {
            return Err(QueryIndexError::InvalidHeader("header size"));
        }
        let expected_checksum = Sha256::digest(&header[..HEADER_CHECKSUM_OFFSET]);
        if header[HEADER_CHECKSUM_OFFSET..] != expected_checksum[..] {
            return Err(QueryIndexError::InvalidHeader("checksum"));
        }
        if get_u64(header, 16)? != mmap.len() as u64 {
            return Err(QueryIndexError::InvalidHeader("total length"));
        }
        if get_u32(header, 40)? as usize != QUERY_INDEX_PAGE_SIZE {
            return Err(QueryIndexError::InvalidHeader("page size"));
        }
        if get_u64(header, 24)? != expected.evidence_bytes {
            return Err(QueryIndexError::IdentityMismatch("evidence length"));
        }
        if get_u32(header, 32)? != expected.archive_schema_version {
            return Err(QueryIndexError::IdentityMismatch("archive schema"));
        }
        if header[48..80] != expected.evidence_sha256 {
            return Err(QueryIndexError::IdentityMismatch("evidence hash"));
        }
        if header[80..112] != expected.analysis_sha256 {
            return Err(QueryIndexError::IdentityMismatch("analysis hash"));
        }
        if header[112..144] != expected.producer_sha256 {
            return Err(QueryIndexError::IdentityMismatch("producer hash"));
        }
        let section_count = get_u32(header, 36)? as usize;
        if section_count > QUERY_INDEX_MAX_SECTIONS {
            return Err(QueryIndexError::TooManySections(section_count));
        }
        let mut descriptors = Vec::with_capacity(section_count);
        let mut kinds = HashSet::new();
        let mut ranges = vec![(0_u64, QUERY_INDEX_HEADER_SIZE as u64)];
        for index in 0..section_count {
            let offset = DESCRIPTOR_OFFSET + index * DESCRIPTOR_SIZE;
            let descriptor = SectionDescriptor {
                kind: get_u32(header, offset)?,
                record_size: get_u32(header, offset + 4)?,
                offset: get_u64(header, offset + 8)?,
                length: get_u64(header, offset + 16)?,
                count: get_u64(header, offset + 24)?,
                digests_offset: get_u64(header, offset + 32)?,
                digest_count: get_u32(header, offset + 40)?,
            };
            if descriptor.kind == 0 || !kinds.insert(descriptor.kind) {
                return Err(QueryIndexError::DuplicateSection(descriptor.kind));
            }
            if descriptor.record_size > 0
                && u64::from(descriptor.record_size)
                    .checked_mul(descriptor.count)
                    .ok_or(QueryIndexError::SizeOverflow)?
                    != descriptor.length
            {
                return Err(QueryIndexError::InvalidSection {
                    kind: descriptor.kind,
                    reason: "record shape",
                });
            }
            let digest_count = (descriptor.length as usize).div_ceil(QUERY_INDEX_PAGE_SIZE);
            if descriptor.digest_count as usize != digest_count {
                return Err(QueryIndexError::InvalidSection {
                    kind: descriptor.kind,
                    reason: "digest count",
                });
            }
            let data_end = checked_end(descriptor.offset, descriptor.length)?;
            let digest_length = u64::from(descriptor.digest_count) * 32;
            let digest_end = checked_end(descriptor.digests_offset, digest_length)?;
            if descriptor.offset < QUERY_INDEX_HEADER_SIZE as u64
                || data_end > mmap.len() as u64
                || descriptor.digests_offset < QUERY_INDEX_HEADER_SIZE as u64
                || digest_end > mmap.len() as u64
            {
                return Err(QueryIndexError::InvalidSection {
                    kind: descriptor.kind,
                    reason: "bounds",
                });
            }
            ranges.push((descriptor.offset, data_end));
            ranges.push((descriptor.digests_offset, digest_end));
            descriptors.push(descriptor);
        }
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(QueryIndexError::InvalidHeader("overlapping sections"));
        }
        Ok(Self {
            mmap,
            descriptors,
            verified_pages: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn descriptor(&self, kind: u32) -> Result<SectionDescriptor, QueryIndexError> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.kind == kind)
            .copied()
            .ok_or(QueryIndexError::MissingSection(kind))
    }

    /// Authenticate every section page before a disposable index is accepted.
    pub fn verify_all(&self) -> Result<(), QueryIndexError> {
        for descriptor in &self.descriptors {
            for page in 0..descriptor.digest_count as usize {
                self.verify_page(*descriptor, page)?;
            }
        }
        Ok(())
    }

    fn verify_page(
        &self,
        descriptor: SectionDescriptor,
        page: usize,
    ) -> Result<(), QueryIndexError> {
        let key = (descriptor.kind, page);
        if self.verified_pages.lock().unwrap().contains(&key) {
            return Ok(());
        }
        let page_start = (descriptor.offset as usize)
            .checked_add(page * QUERY_INDEX_PAGE_SIZE)
            .ok_or(QueryIndexError::SizeOverflow)?;
        let section_end = checked_end(descriptor.offset, descriptor.length)? as usize;
        let page_end = page_start
            .checked_add(QUERY_INDEX_PAGE_SIZE)
            .ok_or(QueryIndexError::SizeOverflow)?
            .min(section_end);
        let digest_start = (descriptor.digests_offset as usize)
            .checked_add(page * 32)
            .ok_or(QueryIndexError::SizeOverflow)?;
        let expected =
            self.mmap
                .get(digest_start..digest_start + 32)
                .ok_or(QueryIndexError::CorruptPage {
                    kind: descriptor.kind,
                    page,
                })?;
        let actual = Sha256::digest(&self.mmap[page_start..page_end]);
        if actual[..] != expected[..] {
            return Err(QueryIndexError::CorruptPage {
                kind: descriptor.kind,
                page,
            });
        }
        self.verified_pages.lock().unwrap().insert(key);
        Ok(())
    }

    pub fn bytes(&self, kind: u32, offset: u64, length: u64) -> Result<&[u8], QueryIndexError> {
        let descriptor = self.descriptor(kind)?;
        let end = checked_end(offset, length)?;
        if end > descriptor.length {
            return Err(QueryIndexError::OutOfBounds {
                kind,
                offset,
                length,
            });
        }
        if length > 0 {
            let first = offset as usize / QUERY_INDEX_PAGE_SIZE;
            let last = (end - 1) as usize / QUERY_INDEX_PAGE_SIZE;
            for page in first..=last {
                self.verify_page(descriptor, page)?;
            }
        }
        let start = (descriptor.offset + offset) as usize;
        let end = (descriptor.offset + end) as usize;
        Ok(&self.mmap[start..end])
    }

    pub fn record(&self, kind: u32, index: u64) -> Result<&[u8], QueryIndexError> {
        let descriptor = self.descriptor(kind)?;
        if descriptor.record_size == 0 || index >= descriptor.count {
            return Err(QueryIndexError::OutOfBounds {
                kind,
                offset: index,
                length: 1,
            });
        }
        let offset = index
            .checked_mul(u64::from(descriptor.record_size))
            .ok_or(QueryIndexError::SizeOverflow)?;
        self.bytes(kind, offset, u64::from(descriptor.record_size))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn root(label: &str) -> PathBuf {
        // The counter keeps two tests that read the same clock tick apart.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-query-index-{label}-{}-{}-{nonce}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn identity(seed: u8) -> QueryIndexIdentity {
        QueryIndexIdentity {
            evidence_sha256: [seed; 32],
            evidence_bytes: 100 + u64::from(seed),
            analysis_sha256: [seed.wrapping_add(1); 32],
            producer_sha256: [seed.wrapping_add(2); 32],
            archive_schema_version: 2,
        }
    }

    fn sections(value: u8) -> Vec<QueryIndexSection> {
        vec![
            QueryIndexSection {
                kind: 2,
                record_size: 4,
                count: 2,
                bytes: vec![value, 1, 2, 3, value, 5, 6, 7],
            },
            QueryIndexSection {
                kind: 1,
                record_size: 0,
                count: 3,
                bytes: vec![8; QUERY_INDEX_PAGE_SIZE + 3],
            },
        ]
    }

    #[test]
    fn atomically_round_trips_sections_and_checked_records() {
        let root = root("roundtrip");
        let path = root.join("query-index.v1.bin");
        write_query_index(&sections(9), &identity(1), &path).unwrap();
        let index = QueryIndex::open(&path, &identity(1)).unwrap();
        assert_eq!(index.record(2, 0).unwrap(), &[9, 1, 2, 3]);
        assert_eq!(index.record(2, 1).unwrap(), &[9, 5, 6, 7]);
        assert_eq!(
            index.bytes(1, QUERY_INDEX_PAGE_SIZE as u64 - 1, 4).unwrap(),
            &[8; 4]
        );
        assert!(matches!(
            index.record(2, 2),
            Err(QueryIndexError::OutOfBounds { .. })
        ));
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_stale_identity_and_payload_corruption() {
        let root = root("corrupt");
        let path = root.join("query-index.v1.bin");
        write_query_index(&sections(9), &identity(1), &path).unwrap();
        assert!(matches!(
            QueryIndex::open(&path, &identity(2)),
            Err(QueryIndexError::IdentityMismatch(_))
        ));
        let descriptor = QueryIndex::open(&path, &identity(1))
            .unwrap()
            .descriptor(2)
            .unwrap();
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(descriptor.offset)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
        let index = QueryIndex::open(&path, &identity(1)).unwrap();
        assert!(matches!(
            index.record(2, 0),
            Err(QueryIndexError::CorruptPage { kind: 2, page: 0 })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_never_invalidates_an_open_mapping() {
        let root = root("replacement");
        let path = root.join("query-index.v1.bin");
        write_query_index(&sections(1), &identity(1), &path).unwrap();
        let old = QueryIndex::open(&path, &identity(1)).unwrap();
        write_query_index(&sections(2), &identity(2), &path).unwrap();
        assert_eq!(old.record(2, 0).unwrap()[0], 1);
        assert_eq!(
            QueryIndex::open(&path, &identity(2))
                .unwrap()
                .record(2, 0)
                .unwrap()[0],
            2
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_duplicate_and_misshapen_sections_without_publication() {
        let root = root("invalid");
        let path = root.join("query-index.v1.bin");
        let duplicate = vec![
            QueryIndexSection {
                kind: 1,
                record_size: 1,
                count: 1,
                bytes: vec![1],
            },
            QueryIndexSection {
                kind: 1,
                record_size: 1,
                count: 1,
                bytes: vec![2],
            },
        ];
        assert!(matches!(
            write_query_index(&duplicate, &identity(1), &path),
            Err(QueryIndexError::DuplicateSection(1))
        ));
        assert!(!path.exists());
        assert!(matches!(
            write_query_index(
                &[QueryIndexSection {
                    kind: 1,
                    record_size: 4,
                    count: 2,
                    bytes: vec![1]
                }],
                &identity(1),
                &path
            ),
            Err(QueryIndexError::InvalidSection { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_too_many_sections_and_the_reserved_kind() {
        let root = root("refused-sections");
        let path = root.join("query-index.v1.bin");
        let many = (1..=QUERY_INDEX_MAX_SECTIONS as u32 + 1)
            .map(|kind| QueryIndexSection {
                kind,
                record_size: 0,
                count: 0,
                bytes: Vec::new(),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            write_query_index(&many, &identity(1), &path),
            Err(QueryIndexError::TooManySections(49))
        ));
        assert!(matches!(
            write_query_index(&many[..0], &identity(1), &path),
            Ok(())
        ));
        assert!(matches!(
            write_query_index(
                &[QueryIndexSection {
                    kind: 0,
                    record_size: 0,
                    count: 0,
                    bytes: Vec::new(),
                }],
                &identity(1),
                &path
            ),
            Err(QueryIndexError::InvalidSection { kind: 0, .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_section_without_records_has_no_record_to_read() {
        let root = root("no-records");
        let path = root.join("query-index.v1.bin");
        write_query_index(&sections(9), &identity(1), &path).unwrap();
        let index = QueryIndex::open(&path, &identity(1)).unwrap();
        assert!(matches!(
            index.record(1, 0),
            Err(QueryIndexError::OutOfBounds { kind: 1, .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_temporary_name_already_taken_is_passed_over() {
        let root = root("taken");
        let path = root.join("query-index.v1.bin");
        let next = TEMPORARY_SEQUENCE.load(Ordering::Relaxed);
        let taken = (next..next + 256)
            .map(|sequence| temporary_path(&path, sequence).unwrap())
            .collect::<Vec<_>>();
        for temporary in &taken {
            fs::write(temporary, b"another writer's").unwrap();
        }
        write_query_index(&sections(9), &identity(1), &path).unwrap();
        QueryIndex::open(&path, &identity(1)).unwrap();
        for temporary in &taken {
            assert_eq!(fs::read(temporary).unwrap(), b"another writer's");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file_behind() {
        let root = root("failed-write");
        // A temporary name too long to create fails before anything is written.
        let long = root.join("q".repeat(250));
        assert!(matches!(
            write_query_index(&sections(9), &identity(1), &long),
            Err(QueryIndexError::Io(_))
        ));
        // A destination that is a directory cannot be renamed over.
        let path = root.join("query-index.v1.bin");
        fs::create_dir_all(path.join("occupied")).unwrap();
        assert!(matches!(
            write_query_index(&sections(9), &identity(1), &path),
            Err(QueryIndexError::Io(_))
        ));
        let names = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["query-index.v1.bin"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_indexes() {
        use std::os::unix::fs::symlink;

        let root = root("symlink");
        let path = root.join("query-index.v1.bin");
        let link = root.join("linked.bin");
        write_query_index(&sections(1), &identity(1), &path).unwrap();
        symlink(&path, &link).unwrap();
        assert!(matches!(
            QueryIndex::open(&link, &identity(1)),
            Err(QueryIndexError::NotRegularFile(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    /// Every field of the header and of each section descriptor is checked:
    /// an index whose header is re-signed after one field changed is refused
    /// for that field.
    #[test]
    fn each_header_and_descriptor_field_is_checked() {
        let root = root("fields");
        let path = root.join("query-index.v1.bin");
        write_query_index(&sections(9), &identity(1), &path).unwrap();
        let valid = fs::read(&path).unwrap();
        let open_with = |bytes: &[u8], expected: &QueryIndexIdentity| {
            fs::write(&path, bytes).unwrap();
            match QueryIndex::open(&path, expected) {
                Ok(_) => "accepted".to_owned(),
                Err(error) => format!("{error:?}"),
            }
        };
        let open = |bytes: &[u8]| open_with(bytes, &identity(1));
        let signed = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = valid.clone();
            edit(&mut bytes);
            let checksum = Sha256::digest(&bytes[..HEADER_CHECKSUM_OFFSET]);
            bytes[HEADER_CHECKSUM_OFFSET..QUERY_INDEX_HEADER_SIZE].copy_from_slice(&checksum);
            bytes
        };
        let put32 = |bytes: &mut Vec<u8>, at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        let put64 = |bytes: &mut Vec<u8>, at: usize, value: u64| {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
        };
        let field =
            |section: usize, offset: usize| DESCRIPTOR_OFFSET + section * DESCRIPTOR_SIZE + offset;
        let read64 = |at: usize| u64::from_le_bytes(valid[at..at + 8].try_into().unwrap());
        assert_eq!(open(&valid), "accepted");
        // The descriptor whose records have a fixed size.
        let sized = (0..2)
            .find(|&section| valid[field(section, 4)..field(section, 4) + 4] != [0; 4])
            .unwrap();
        let mut longer = valid.clone();
        longer.extend_from_slice(&[0; 8]);
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (valid[..100].to_vec(), "InvalidHeader(\"truncated\")"),
            (signed(&|b| b[0] = b'X'), "InvalidHeader(\"magic\")"),
            (
                signed(&|b| put32(b, 8, 99)),
                "InvalidHeader(\"schema version\")",
            ),
            (
                signed(&|b| put32(b, 12, 1)),
                "InvalidHeader(\"header size\")",
            ),
            (
                {
                    let mut b = valid.clone();
                    b[200] ^= 1;
                    b
                },
                "InvalidHeader(\"checksum\")",
            ),
            (longer, "InvalidHeader(\"total length\")"),
            (signed(&|b| put32(b, 40, 1)), "InvalidHeader(\"page size\")"),
            (
                signed(&|b| put32(b, 36, QUERY_INDEX_MAX_SECTIONS as u32 + 1)),
                "TooManySections",
            ),
            (signed(&|b| put32(b, field(0, 0), 0)), "DuplicateSection(0)"),
            (
                signed(&|b| {
                    let kind =
                        u32::from_le_bytes(b[field(0, 0)..field(0, 0) + 4].try_into().unwrap());
                    put32(b, field(1, 0), kind)
                }),
                "DuplicateSection",
            ),
            (signed(&|b| put64(b, field(sized, 24), 3)), "record shape"),
            (signed(&|b| put32(b, field(0, 40), 7)), "digest count"),
            (signed(&|b| put64(b, field(0, 8), 8)), "bounds"),
            (
                signed(&|b| put64(b, field(0, 8), valid.len() as u64)),
                "bounds",
            ),
            (signed(&|b| put64(b, field(0, 32), 8)), "bounds"),
            (
                signed(&|b| put64(b, field(0, 32), valid.len() as u64)),
                "bounds",
            ),
            (
                signed(&|b| put64(b, field(1, 8), read64(field(0, 8)))),
                "overlapping sections",
            ),
        ];
        for (bytes, expected) in &cases {
            let said = open(bytes);
            assert!(said.contains(expected), "{expected}: {said}");
        }
        for (identity, reason) in [
            (
                QueryIndexIdentity {
                    evidence_bytes: 1,
                    ..identity(1)
                },
                "evidence length",
            ),
            (
                QueryIndexIdentity {
                    archive_schema_version: 9,
                    ..identity(1)
                },
                "archive schema",
            ),
            (
                QueryIndexIdentity {
                    evidence_sha256: [0; 32],
                    ..identity(1)
                },
                "evidence hash",
            ),
            (
                QueryIndexIdentity {
                    analysis_sha256: [0; 32],
                    ..identity(1)
                },
                "analysis hash",
            ),
            (
                QueryIndexIdentity {
                    producer_sha256: [0; 32],
                    ..identity(1)
                },
                "producer hash",
            ),
        ] {
            let said = open_with(&valid, &identity);
            assert!(said.contains(reason), "{reason}: {said}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn each_query_index_error_says_what_failed() {
        let messages = [
            QueryIndexError::from(io::Error::other("disk full")),
            QueryIndexError::NotRegularFile(PathBuf::from("index")),
            QueryIndexError::InvalidHeader("magic"),
            QueryIndexError::IdentityMismatch("evidence digest"),
            QueryIndexError::TooManySections(99),
            QueryIndexError::InvalidSection {
                kind: 3,
                reason: "record size",
            },
            QueryIndexError::DuplicateSection(3),
            QueryIndexError::MissingSection(4),
            QueryIndexError::OutOfBounds {
                kind: 5,
                offset: 8,
                length: 16,
            },
            QueryIndexError::CorruptPage { kind: 6, page: 2 },
            QueryIndexError::SizeOverflow,
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "disk full",
                "query index is not a regular file: index",
                "invalid query-index header: magic",
                "stale query-index evidence digest",
                "query index has 99 sections",
                "invalid query-index section 3: record size",
                "duplicate query-index section 3",
                "missing query-index section 4",
                "query-index section 5 range 8+16 is out of bounds",
                "query-index section 6 page 2 is corrupt",
                "query index exceeds its format limits",
            ]
        );
    }
}
