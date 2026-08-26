//! Bounded, lock-free, file-backed transport for owned Rust probe events.
//!
//! The supervisor creates and authenticates the fixed-layout file before a
//! test starts. Target code maps that file and publishes variable-length
//! records through fixed descriptors. A release-store commit byte makes every
//! complete descriptor independently recoverable even if another writer or
//! the whole process dies midway through a later record.

use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use memmap2::{Mmap, MmapMut, MmapOptions};

use crate::rust_runtime::{RustProbeObservation, valid_probe_id};

pub const RUST_TRANSPORT_ENV: &str = "SUPERCOV_RUST_TRANSPORT_FILE";
pub const RUST_TRANSPORT_TOKEN_ENV: &str = "SUPERCOV_RUST_TRANSPORT_TOKEN";
pub const RUST_CONTEXT_ENV: &str = "SUPERCOV_RUST_CONTEXT_ID";
pub const DEFAULT_DESCRIPTOR_CAPACITY: u32 = 32_768;
pub const DEFAULT_PAYLOAD_CAPACITY: u32 = 4 * 1024 * 1024;

const MAGIC: &[u8; 8] = b"SCVRUST1";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 128;
const DESCRIPTOR_SIZE: usize = 40;
const ENDIAN_MARKER: u32 = 0x0102_0304;
const NEXT_DESCRIPTOR_OFFSET: usize = 32;
const NEXT_PAYLOAD_OFFSET: usize = 40;
const DROPPED_OFFSET: usize = 48;
const TOKEN_OFFSET: usize = 56;
const TOKEN_SIZE: usize = 16;
const ATTACHMENTS_OFFSET: usize = 72;

const COMMIT_OFFSET: usize = 0;
const KIND_OFFSET: usize = 1;
const OUTCOME_OFFSET: usize = 2;
const PID_OFFSET: usize = 4;
const CONTEXT_OFFSET: usize = 8;
const PAYLOAD_OFFSET_OFFSET: usize = 16;
const PAYLOAD_LENGTH_OFFSET: usize = 20;
const ID_LENGTH_OFFSET: usize = 24;
const VALUE_LENGTH_OFFSET: usize = 28;
const CHECKSUM_OFFSET: usize = 32;

const KIND_HIT: u8 = 1;
const KIND_DECISION: u8 = 2;
const KIND_ORDINAL_HIT: u8 = 3;

const RUNTIME_TEMPLATE: &str = include_str!("../runtime-assets/rust-mmap-runtime.rs");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustTransportRead {
    pub observations: Vec<RustTransportObservation>,
    pub ordinal_hits: Vec<RustOrdinalHit>,
    pub committed: u64,
    pub incomplete: u64,
    pub dropped: u64,
    pub attachments: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustTransportObservation {
    pub process_id: u32,
    pub context_id: u64,
    pub observation: RustProbeObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RustOrdinalHit {
    pub process_id: u32,
    pub context_id: u64,
    pub ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustTransportError {
    Io(String),
    UnsafeFile(String),
    InvalidHeader,
    InvalidLength,
    InvalidDescriptor(u64),
    InvalidRecord(u64),
}

impl std::fmt::Display for RustTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Rust transport I/O failed: {error}"),
            Self::UnsafeFile(path) => write!(formatter, "unsafe Rust transport file: {path}"),
            Self::InvalidHeader => write!(formatter, "invalid Rust transport header"),
            Self::InvalidLength => write!(formatter, "invalid Rust transport length"),
            Self::InvalidDescriptor(index) => {
                write!(formatter, "invalid Rust transport descriptor {index}")
            }
            Self::InvalidRecord(index) => {
                write!(formatter, "invalid Rust transport record {index}")
            }
        }
    }
}

impl std::error::Error for RustTransportError {}

fn put_u32(target: &mut [u8], offset: usize, value: u32) {
    target[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(source: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        source.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn get_u64(source: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        source.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn total_size(descriptors: u32, payload: u32) -> Option<usize> {
    HEADER_SIZE
        .checked_add(
            usize::try_from(descriptors)
                .ok()?
                .checked_mul(DESCRIPTOR_SIZE)?,
        )?
        .checked_add(usize::try_from(payload).ok()?)
}

fn map_mut(file: &File) -> Result<MmapMut, RustTransportError> {
    // SAFETY: this function owns the newly created file mapping and no slice
    // alias is produced outside the returned MmapMut.
    unsafe { MmapOptions::new().map_mut(file) }
        .map_err(|error| RustTransportError::Io(error.to_string()))
}

fn map(file: &File) -> Result<Mmap, RustTransportError> {
    // SAFETY: the immutable mapping is retained for the duration of all reads.
    unsafe { MmapOptions::new().map(file) }
        .map_err(|error| RustTransportError::Io(error.to_string()))
}

pub fn create_rust_transport(
    path: &Path,
    token: [u8; TOKEN_SIZE],
    descriptor_capacity: u32,
    payload_capacity: u32,
) -> Result<(), RustTransportError> {
    if descriptor_capacity == 0 || payload_capacity == 0 {
        return Err(RustTransportError::InvalidLength);
    }
    let total = total_size(descriptor_capacity, payload_capacity)
        .ok_or(RustTransportError::InvalidLength)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|error| RustTransportError::Io(error.to_string()))?;
    file.set_len(u64::try_from(total).map_err(|_| RustTransportError::InvalidLength)?)
        .map_err(|error| RustTransportError::Io(error.to_string()))?;
    let mut mapping = map_mut(&file)?;
    mapping[..MAGIC.len()].copy_from_slice(MAGIC);
    put_u32(&mut mapping, 8, VERSION);
    put_u32(&mut mapping, 12, HEADER_SIZE as u32);
    put_u32(&mut mapping, 16, DESCRIPTOR_SIZE as u32);
    put_u32(&mut mapping, 20, descriptor_capacity);
    put_u32(&mut mapping, 24, payload_capacity);
    put_u32(&mut mapping, 28, ENDIAN_MARKER);
    mapping[TOKEN_OFFSET..TOKEN_OFFSET + TOKEN_SIZE].copy_from_slice(&token);
    mapping
        .flush_range(0, HEADER_SIZE)
        .map_err(|error| RustTransportError::Io(error.to_string()))
}

fn regular_file(path: &Path) -> Result<File, RustTransportError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| RustTransportError::Io(error.to_string()))?;
    if !metadata.file_type().is_file() {
        return Err(RustTransportError::UnsafeFile(path.display().to_string()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(target_os = "linux")]
        const O_NOFOLLOW: i32 = 0x2_0000;
        #[cfg(target_os = "macos")]
        const O_NOFOLLOW: i32 = 0x100;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| RustTransportError::Io(error.to_string()))?;
    if !file
        .metadata()
        .map_err(|error| RustTransportError::Io(error.to_string()))?
        .file_type()
        .is_file()
    {
        return Err(RustTransportError::UnsafeFile(path.display().to_string()));
    }
    Ok(file)
}

fn atomic_u64(mapping: &[u8], offset: usize) -> Result<&AtomicU64, RustTransportError> {
    let pointer = mapping
        .get(offset..offset + 8)
        .ok_or(RustTransportError::InvalidHeader)?
        .as_ptr();
    if !(pointer as usize).is_multiple_of(std::mem::align_of::<AtomicU64>()) {
        return Err(RustTransportError::InvalidHeader);
    }
    // SAFETY: the fixed layout guarantees alignment and the mapped bytes live
    // at least as long as the returned reference.
    Ok(unsafe { &*pointer.cast::<AtomicU64>() })
}

fn atomic_u8(mapping: &[u8], offset: usize) -> Result<&AtomicU8, RustTransportError> {
    let pointer = mapping
        .get(offset)
        .ok_or(RustTransportError::InvalidLength)? as *const u8;
    // SAFETY: AtomicU8 has byte alignment and the mapping outlives the reference.
    Ok(unsafe { &*pointer.cast::<AtomicU8>() })
}

#[allow(clippy::too_many_arguments)]
fn checksum(
    kind: u8,
    outcome: u8,
    pid: u32,
    context: u64,
    payload_offset: u32,
    payload_length: u32,
    id_length: u32,
    value_length: u32,
    id: &[u8],
    values: &[u8],
) -> u64 {
    let mut value = 0xcbf2_9ce4_8422_2325_u64;
    for byte in [kind, outcome]
        .into_iter()
        .chain(pid.to_le_bytes())
        .chain(context.to_le_bytes())
        .chain(payload_offset.to_le_bytes())
        .chain(payload_length.to_le_bytes())
        .chain(id_length.to_le_bytes())
        .chain(value_length.to_le_bytes())
        .chain(id.iter().copied())
        .chain(values.iter().copied())
    {
        value ^= u64::from(byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    value
}

pub fn read_rust_transport(
    path: &Path,
    expected_token: &[u8; TOKEN_SIZE],
) -> Result<RustTransportRead, RustTransportError> {
    let file = regular_file(path)?;
    let mapping = map(&file)?;
    if mapping.get(..8) != Some(MAGIC.as_slice())
        || get_u32(&mapping, 8) != Some(VERSION)
        || get_u32(&mapping, 12) != Some(HEADER_SIZE as u32)
        || get_u32(&mapping, 16) != Some(DESCRIPTOR_SIZE as u32)
        || get_u32(&mapping, 28) != Some(ENDIAN_MARKER)
        || mapping.get(TOKEN_OFFSET..TOKEN_OFFSET + TOKEN_SIZE) != Some(expected_token.as_slice())
        || mapping.get(52..56).is_none_or(|bytes| bytes != [0; 4])
        || mapping
            .get(80..HEADER_SIZE)
            .is_none_or(|bytes| bytes != [0; 48])
    {
        return Err(RustTransportError::InvalidHeader);
    }
    let descriptors = get_u32(&mapping, 20).ok_or(RustTransportError::InvalidHeader)?;
    let payload_capacity = get_u32(&mapping, 24).ok_or(RustTransportError::InvalidHeader)?;
    if descriptors == 0 || payload_capacity == 0 {
        return Err(RustTransportError::InvalidHeader);
    }
    if mapping.len()
        != total_size(descriptors, payload_capacity).ok_or(RustTransportError::InvalidLength)?
    {
        return Err(RustTransportError::InvalidLength);
    }
    let next = atomic_u64(&mapping, NEXT_DESCRIPTOR_OFFSET)?.load(Ordering::Acquire);
    let next_payload = atomic_u64(&mapping, NEXT_PAYLOAD_OFFSET)?.load(Ordering::Acquire);
    let recorded_dropped = atomic_u64(&mapping, DROPPED_OFFSET)?.load(Ordering::Acquire);
    let attachments = atomic_u64(&mapping, ATTACHMENTS_OFFSET)?.load(Ordering::Acquire);
    let inspect = next.min(u64::from(descriptors));
    let payload_base = HEADER_SIZE + descriptors as usize * DESCRIPTOR_SIZE;
    let mut observations = Vec::new();
    let mut ordinal_hits = Vec::new();
    let mut committed = 0_u64;
    for index in 0..inspect {
        let descriptor = HEADER_SIZE + index as usize * DESCRIPTOR_SIZE;
        match atomic_u8(&mapping, descriptor + COMMIT_OFFSET)?.load(Ordering::Acquire) {
            0 => continue,
            1 => {}
            _ => return Err(RustTransportError::InvalidDescriptor(index)),
        }
        committed += 1;
        let kind = mapping[descriptor + KIND_OFFSET];
        let outcome = mapping[descriptor + OUTCOME_OFFSET];
        if mapping[descriptor + 3] != 0 {
            return Err(RustTransportError::InvalidDescriptor(index));
        }
        let pid = get_u32(&mapping, descriptor + PID_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?;
        let context_id = get_u64(&mapping, descriptor + CONTEXT_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?;
        let payload_offset = get_u32(&mapping, descriptor + PAYLOAD_OFFSET_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?
            as usize;
        let payload_length = get_u32(&mapping, descriptor + PAYLOAD_LENGTH_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?
            as usize;
        let id_length = get_u32(&mapping, descriptor + ID_LENGTH_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))? as usize;
        let value_length = get_u32(&mapping, descriptor + VALUE_LENGTH_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?
            as usize;
        let expected_checksum = get_u64(&mapping, descriptor + CHECKSUM_OFFSET)
            .ok_or(RustTransportError::InvalidDescriptor(index))?;
        let end = payload_offset
            .checked_add(payload_length)
            .filter(|end| *end <= payload_capacity as usize)
            .ok_or(RustTransportError::InvalidDescriptor(index))?;
        if payload_length != id_length.saturating_add(value_length)
            || u64::from(end as u32) > next_payload
        {
            return Err(RustTransportError::InvalidDescriptor(index));
        }
        let payload = mapping
            .get(payload_base + payload_offset..payload_base + end)
            .ok_or(RustTransportError::InvalidDescriptor(index))?;
        let (id, values) = payload.split_at(id_length);
        if checksum(
            kind,
            outcome,
            pid,
            context_id,
            payload_offset as u32,
            payload_length as u32,
            id_length as u32,
            value_length as u32,
            id,
            values,
        ) != expected_checksum
        {
            return Err(RustTransportError::InvalidRecord(index));
        }
        let id = std::str::from_utf8(id).map_err(|_| RustTransportError::InvalidRecord(index))?;
        if kind != KIND_ORDINAL_HIT && !valid_probe_id(id) {
            return Err(RustTransportError::InvalidRecord(index));
        }
        match kind {
            KIND_HIT if outcome == 0 && values.is_empty() => {
                observations.push(RustTransportObservation {
                    process_id: pid,
                    context_id,
                    observation: RustProbeObservation::Hit { id: id.into() },
                });
            }
            KIND_DECISION
                if matches!(outcome, 0 | 1)
                    && id.starts_with("rs:decision:")
                    && !values.is_empty()
                    && values.iter().all(|value| matches!(*value, 0..=2)) =>
            {
                observations.push(RustTransportObservation {
                    process_id: pid,
                    context_id,
                    observation: RustProbeObservation::Decision {
                        id: id.into(),
                        values: values
                            .iter()
                            .map(|value| match value {
                                0 => None,
                                1 => Some(false),
                                2 => Some(true),
                                _ => unreachable!(),
                            })
                            .collect(),
                        outcome: outcome == 1,
                    },
                });
            }
            KIND_ORDINAL_HIT if outcome == 0 && id.is_empty() && values.len() == 8 => {
                ordinal_hits.push(RustOrdinalHit {
                    process_id: pid,
                    context_id,
                    ordinal: u64::from_le_bytes(
                        values
                            .try_into()
                            .map_err(|_| RustTransportError::InvalidRecord(index))?,
                    ),
                });
            }
            _ => return Err(RustTransportError::InvalidRecord(index)),
        }
    }
    let overflow = next.saturating_sub(u64::from(descriptors));
    Ok(RustTransportRead {
        observations,
        ordinal_hits,
        committed,
        incomplete: inspect.saturating_sub(committed),
        dropped: recorded_dropped.max(overflow),
        attachments,
    })
}

pub fn render_rust_mmap_runtime(module_name: &str) -> Result<String, String> {
    let valid_identifier = !module_name.is_empty()
        && module_name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
    if !valid_identifier {
        return Err("invalid Rust runtime module name".into());
    }
    Ok(RUNTIME_TEMPLATE.replace("__SUPERCOV_MODULE__", module_name))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead as _, BufReader},
        process::{Command, Stdio},
        sync::atomic::Ordering,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    const TOKEN: [u8; TOKEN_SIZE] = [0x42; TOKEN_SIZE];
    const CONTEXT: u64 = 42;

    fn token_hex() -> String {
        TOKEN.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-rust-transport-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn compile_fixture(directory: &Path) -> std::path::PathBuf {
        let source = directory.join("main.rs");
        let binary = directory.join("program");
        let runtime = render_rust_mmap_runtime("__supercov_runtime_v1").unwrap();
        fs::write(
            &source,
            format!(
                r#"{runtime}
fn main() {{
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "threads" {{
        let mut threads = Vec::new();
        for _ in 0..8 {{
            threads.push(std::thread::spawn(|| {{
                for _ in 0..100 {{ __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567"); }}
            }}));
        }}
        for thread in threads {{ thread.join().unwrap(); }}
        let mut frame = __supercov_runtime_v1::DecisionFrame::new("rs:decision:0123456789abcdef01234567", 2);
        let first = __supercov_runtime_v1::condition(true, &mut frame, 0);
        let second = __supercov_runtime_v1::condition(false, &mut frame, 1);
        __supercov_runtime_v1::decision(first && second, &mut frame);
        __supercov_runtime_v1::ordinal_hit(7);
    }} else if mode == "contexts" {{
        __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567");
        let outer = __supercov_runtime_v1::enter_context(100);
        __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567");
        let inner = __supercov_runtime_v1::enter_context(200);
        __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567");
        __supercov_runtime_v1::exit_context(inner);
        __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567");
        __supercov_runtime_v1::exit_context(outer);
        __supercov_runtime_v1::hit("rs:statement:0123456789abcdef01234567");
    }} else if mode == "mir-decisions" {{
        let before_outer = __supercov_runtime_v1::enter_context(901);
        let outer = __supercov_runtime_v1::mir_decision_start(
            0x0123_4567_89ab_cdef,
            0x0123_4567,
            2,
        );
        __supercov_runtime_v1::mir_decision_condition(outer, 0, true);
        let before_inner = __supercov_runtime_v1::enter_context(902);
        let inner = __supercov_runtime_v1::mir_decision_start(
            0xfedc_ba98_7654_3210,
            0xfedc_ba98,
            1,
        );
        let migrated = std::thread::spawn(move || {{
            __supercov_runtime_v1::mir_decision_condition(inner, 0, false);
            __supercov_runtime_v1::mir_decision_finish(inner, false);
        }});
        __supercov_runtime_v1::exit_context(before_inner);
        __supercov_runtime_v1::mir_decision_condition(outer, 1, true);
        __supercov_runtime_v1::exit_context(before_outer);
        __supercov_runtime_v1::mir_decision_finish(outer, true);
        migrated.join().unwrap();
    }} else if mode == "kill" {{
        __supercov_runtime_v1::hit("rs:function:fedcba9876543210fedcba98");
        let interrupted = __supercov_runtime_v1::mir_decision_start(
            0x1111_1111_1111_1111,
            0x2222_2222,
            2,
        );
        __supercov_runtime_v1::mir_decision_condition(interrupted, 0, true);
        println!("ready");
        use std::io::Write as _;
        std::io::stdout().flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }} else {{
        for _ in 0..3 {{ __supercov_runtime_v1::hit("rs:branch:aaaaaaaaaaaaaaaaaaaaaaaa"); }}
    }}
}}
"#
            ),
        )
        .unwrap();
        let output = Command::new("rustc")
            .args(["--edition=2024"])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        binary
    }

    #[test]
    fn implementation_matches_frozen_transport_contract() {
        let contract = supercov_contracts::rust_probe_transport_contract().unwrap();
        assert_eq!(MAGIC.as_slice(), contract.magic.as_bytes());
        assert_eq!(VERSION, contract.protocol_version);
        assert_eq!(HEADER_SIZE, contract.header_size);
        assert_eq!(DESCRIPTOR_SIZE, contract.descriptor_size);
        assert_eq!(TOKEN_SIZE, contract.token_size);
        assert_eq!(ENDIAN_MARKER, contract.endian_marker);
        assert_eq!(
            NEXT_DESCRIPTOR_OFFSET,
            contract.header_offsets.next_descriptor
        );
        assert_eq!(NEXT_PAYLOAD_OFFSET, contract.header_offsets.next_payload);
        assert_eq!(DROPPED_OFFSET, contract.header_offsets.dropped);
        assert_eq!(TOKEN_OFFSET, contract.header_offsets.token);
        assert_eq!(ATTACHMENTS_OFFSET, contract.header_offsets.attachments);
        assert_eq!(COMMIT_OFFSET, contract.descriptor_offsets.commit);
        assert_eq!(PID_OFFSET, contract.descriptor_offsets.process_id);
        assert_eq!(CONTEXT_OFFSET, contract.descriptor_offsets.context_id);
        assert_eq!(
            PAYLOAD_OFFSET_OFFSET,
            contract.descriptor_offsets.payload_offset
        );
        assert_eq!(CHECKSUM_OFFSET, contract.descriptor_offsets.checksum);
        assert_eq!(KIND_HIT, contract.record_kinds.hit);
        assert_eq!(KIND_DECISION, contract.record_kinds.decision);
        assert_eq!(KIND_ORDINAL_HIT, contract.record_kinds.ordinal_hit);
        assert!(RUNTIME_TEMPLATE.contains("b\"SCVRUST1\""));
        assert!(RUNTIME_TEMPLATE.contains("const DESCRIPTOR_SIZE: usize = 40;"));
    }

    #[test]
    fn mmap_transport_is_concurrent_bounded_strict_and_kill_resilient() {
        let directory = temporary_directory("all");
        let binary = compile_fixture(&directory);

        let concurrent = directory.join("concurrent.transport");
        create_rust_transport(&concurrent, TOKEN, 1_024, 128 * 1024).unwrap();
        let output = Command::new(&binary)
            .arg("threads")
            .env(RUST_TRANSPORT_ENV, &concurrent)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&concurrent, &TOKEN).unwrap();
        assert_eq!(read.committed, 802);
        assert_eq!(read.incomplete, 0);
        assert_eq!(read.dropped, 0);
        assert_eq!(read.attachments, 1);
        assert_eq!(read.observations.len(), 801);
        assert!(
            matches!(read.observations.last(), Some(RustTransportObservation { context_id: CONTEXT, observation: RustProbeObservation::Decision { values, outcome: false, .. }, .. }) if values == &[Some(true), Some(false)])
        );
        assert!(
            read.observations
                .iter()
                .all(|item| item.context_id == CONTEXT)
        );
        assert_eq!(read.ordinal_hits.len(), 1);
        assert_eq!(read.ordinal_hits[0].ordinal, 7);
        assert_eq!(read.ordinal_hits[0].context_id, CONTEXT);
        assert_eq!(
            read_rust_transport(&concurrent, &[0x43; TOKEN_SIZE]),
            Err(RustTransportError::InvalidHeader),
            "a supervisor must never accept evidence from another task token"
        );

        let mir_decisions = directory.join("mir-decisions.transport");
        create_rust_transport(&mir_decisions, TOKEN, 16, 4_096).unwrap();
        let output = Command::new(&binary)
            .arg("mir-decisions")
            .env(RUST_TRANSPORT_ENV, &mir_decisions)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&mir_decisions, &TOKEN).unwrap();
        assert_eq!(read.committed, 2);
        assert_eq!(read.dropped, 0);
        assert_eq!(read.incomplete, 0);
        assert!(read.observations.iter().any(|observation| matches!(
            observation,
            RustTransportObservation {
                context_id: 901,
                observation: RustProbeObservation::Decision { id, values, outcome: true },
                ..
            }
                if id == "rs:decision:0123456789abcdef01234567"
                    && values == &[Some(true), Some(true)]
        )));
        assert!(read.observations.iter().any(|observation| matches!(
            observation,
            RustTransportObservation {
                context_id: 902,
                observation: RustProbeObservation::Decision { id, values, outcome: false },
                ..
            }
                if id == "rs:decision:fedcba9876543210fedcba98"
                    && values == &[Some(false)]
        )));

        let processes = directory.join("processes.transport");
        create_rust_transport(&processes, TOKEN, 64, 8 * 1024).unwrap();
        let mut children = (0..8)
            .map(|_| {
                Command::new(&binary)
                    .env(RUST_TRANSPORT_ENV, &processes)
                    .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
                    .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let read = read_rust_transport(&processes, &TOKEN).unwrap();
        assert_eq!(read.attachments, 8);
        assert_eq!(read.committed, 24);
        assert_eq!(read.incomplete, 0);
        assert_eq!(read.dropped, 0);
        assert_eq!(
            read.observations
                .iter()
                .map(|item| item.process_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            8
        );

        let nested = directory.join("nested-context.transport");
        create_rust_transport(&nested, TOKEN, 16, 4_096).unwrap();
        let output = Command::new(&binary)
            .arg("contexts")
            .env(RUST_TRANSPORT_ENV, &nested)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&nested, &TOKEN).unwrap();
        assert_eq!(
            read.observations
                .iter()
                .map(|item| item.context_id)
                .collect::<Vec<_>>(),
            [CONTEXT, 100, 200, 100, CONTEXT]
        );

        let rejected = directory.join("rejected-token.transport");
        create_rust_transport(&rejected, TOKEN, 16, 4_096).unwrap();
        let output = Command::new(&binary)
            .env(RUST_TRANSPORT_ENV, &rejected)
            .env(RUST_TRANSPORT_TOKEN_ENV, "00".repeat(TOKEN_SIZE))
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&rejected, &TOKEN).unwrap();
        assert_eq!(read.attachments, 0);
        assert_eq!(read.committed, 0);
        assert_eq!(read.dropped, 0);

        let malformed_context = directory.join("malformed-context.transport");
        create_rust_transport(&malformed_context, TOKEN, 16, 4_096).unwrap();
        let output = Command::new(&binary)
            .env(RUST_TRANSPORT_ENV, &malformed_context)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, "not-a-context")
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&malformed_context, &TOKEN).unwrap();
        assert_eq!(read.attachments, 0);
        assert_eq!(read.committed, 0);

        let bounded = directory.join("bounded.transport");
        create_rust_transport(&bounded, TOKEN, 2, 256).unwrap();
        let output = Command::new(&binary)
            .env(RUST_TRANSPORT_ENV, &bounded)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&bounded, &TOKEN).unwrap();
        assert_eq!(read.committed, 2);
        assert_eq!(read.dropped, 1);

        let payload_bounded = directory.join("payload-bounded.transport");
        create_rust_transport(&payload_bounded, TOKEN, 8, 16).unwrap();
        let output = Command::new(&binary)
            .env(RUST_TRANSPORT_ENV, &payload_bounded)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let read = read_rust_transport(&payload_bounded, &TOKEN).unwrap();
        assert_eq!(read.committed, 0);
        assert_eq!(read.incomplete, 3);
        assert_eq!(read.dropped, 3);

        let killed = directory.join("killed.transport");
        create_rust_transport(&killed, TOKEN, 16, 4_096).unwrap();
        let mut child = Command::new(&binary)
            .arg("kill")
            .env(RUST_TRANSPORT_ENV, &killed)
            .env(RUST_TRANSPORT_TOKEN_ENV, token_hex())
            .env(RUST_CONTEXT_ENV, format!("{CONTEXT:016x}"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        child.kill().unwrap();
        child.wait().unwrap();
        let read = read_rust_transport(&killed, &TOKEN).unwrap();
        assert_eq!(read.committed, 1);
        assert_eq!(read.incomplete, 1);
        assert_eq!(read.dropped, 0);
        assert_eq!(
            read.observations,
            [RustTransportObservation {
                process_id: read.observations[0].process_id,
                context_id: CONTEXT,
                observation: RustProbeObservation::Hit {
                    id: "rs:function:fedcba9876543210fedcba98".into()
                }
            }]
        );

        let corrupt = directory.join("corrupt.transport");
        create_rust_transport(&corrupt, TOKEN, 2, 256).unwrap();
        let mut bytes = fs::read(&corrupt).unwrap();
        bytes[0] ^= 1;
        fs::write(&corrupt, bytes).unwrap();
        assert_eq!(
            read_rust_transport(&corrupt, &TOKEN),
            Err(RustTransportError::InvalidHeader)
        );

        let invalid_commit = directory.join("invalid-commit.transport");
        fs::copy(&bounded, &invalid_commit).unwrap();
        let mut bytes = fs::read(&invalid_commit).unwrap();
        bytes[HEADER_SIZE + COMMIT_OFFSET] = 2;
        fs::write(&invalid_commit, bytes).unwrap();
        assert_eq!(
            read_rust_transport(&invalid_commit, &TOKEN),
            Err(RustTransportError::InvalidDescriptor(0))
        );

        let invalid_flags = directory.join("invalid-flags.transport");
        fs::copy(&bounded, &invalid_flags).unwrap();
        let mut bytes = fs::read(&invalid_flags).unwrap();
        bytes[HEADER_SIZE + 3] = 1;
        fs::write(&invalid_flags, bytes).unwrap();
        assert_eq!(
            read_rust_transport(&invalid_flags, &TOKEN),
            Err(RustTransportError::InvalidDescriptor(0))
        );

        let invalid_checksum = directory.join("invalid-checksum.transport");
        fs::copy(&bounded, &invalid_checksum).unwrap();
        let mut bytes = fs::read(&invalid_checksum).unwrap();
        bytes[HEADER_SIZE + CHECKSUM_OFFSET] ^= 1;
        fs::write(&invalid_checksum, bytes).unwrap();
        assert_eq!(
            read_rust_transport(&invalid_checksum, &TOKEN),
            Err(RustTransportError::InvalidRecord(0))
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let linked = directory.join("linked.transport");
            symlink(&bounded, &linked).unwrap();
            assert_eq!(
                read_rust_transport(&linked, &TOKEN),
                Err(RustTransportError::UnsafeFile(linked.display().to_string()))
            );
        }

        let truncated = directory.join("truncated.transport");
        create_rust_transport(&truncated, TOKEN, 2, 256).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&truncated)
            .unwrap()
            .set_len(63)
            .unwrap();
        assert_eq!(
            read_rust_transport(&truncated, &TOKEN),
            Err(RustTransportError::InvalidHeader)
        );

        let incomplete = directory.join("incomplete.transport");
        create_rust_transport(&incomplete, TOKEN, 2, 256).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&incomplete)
            .unwrap();
        let mapping = map_mut(&file).unwrap();
        atomic_u64(&mapping, NEXT_DESCRIPTOR_OFFSET)
            .unwrap()
            .store(1, Ordering::Release);
        mapping.flush().unwrap();
        drop(mapping);
        let read = read_rust_transport(&incomplete, &TOKEN).unwrap();
        assert_eq!(read.committed, 0);
        assert_eq!(read.incomplete, 1);
        assert!(read.observations.is_empty());
        assert!(read.ordinal_hits.is_empty());

        fs::remove_dir_all(directory).unwrap();
    }
}
