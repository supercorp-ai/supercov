//! The region runtime for owned Rust probes, and its strict reader.
//!
//! Each instrumented crate carries a copy of the runtime
//! (`runtime-assets/rust-region-runtime.rs`); every copy in a process maps
//! one region, a slot per probe holding its first sighting's sequence and
//! thread. The runtime never computes coverage: the reader rebuilds the
//! records in execution order and the evidence model takes them from there.

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RustProbeObservation {
    Hit {
        id: String,
    },
    Decision {
        id: String,
        values: Vec<Option<bool>>,
        outcome: bool,
    },
    /// Control passed a statement that asserts. A failing assertion panics,
    /// so reaching the marker means it held.
    Assertion {
        id: String,
    },
}

/// One record a process wrote, with the thread that wrote it. Records come
/// back in execution order, but a test that spawns threads interleaves them:
/// an assertion witnesses its own thread's records and no other's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustProbeEntry {
    pub thread: u64,
    pub observation: RustProbeObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustProbeReadError {
    Io(String),
    UnsafeEntry(String),
    InvalidHeader,
    InvalidRecord(usize),
}

impl std::fmt::Display for RustProbeReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Rust probe I/O failed: {error}"),
            Self::UnsafeEntry(path) => write!(formatter, "unsafe Rust probe entry: {path}"),
            Self::InvalidHeader => write!(formatter, "invalid Rust probe header"),
            Self::InvalidRecord(line) => {
                write!(formatter, "invalid Rust probe record at line {line}")
            }
        }
    }
}

impl std::error::Error for RustProbeReadError {}

const REGION_TEMPLATE: &str = include_str!("../runtime-assets/rust-region-runtime.rs");
const REGION_MAGIC: &[u8; 8] = b"SCVRGN01";
const REGION_VERSION: u32 = 1;

/// The region runtime for one crate root. Every crate of an instrumentation
/// shares `token` and `layout`, so their copies map one region per process.
/// `crate_key` names the crate's exports where the region is static memory
/// (wasm): each crate of a module keeps its own there.
pub fn render_rust_region_runtime(
    module_name: &str,
    token: &str,
    crate_key: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
    // The ordinals of the `cfgs` in this crate's own files.
    crate_cfgs: &[usize],
) -> Result<String, String> {
    let valid_identifier = !module_name.is_empty()
        && module_name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
    if !valid_identifier {
        return Err("invalid Rust runtime module name".into());
    }
    if token.is_empty() || token.len() > 16 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Rust region token".into());
    }
    if crate_key.is_empty() || !crate_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Rust runtime crate key".into());
    }
    let mut table = String::new();
    for ordinal in crate_cfgs {
        let gate = layout
            .cfgs
            .get(*ordinal)
            .ok_or("a crate names a cfg gate the layout lacks")?;
        // Re-parsed, so only a predicate `cfg!` accepts reaches the source.
        let predicate = gate
            .predicate
            .parse::<cargo_platform::CfgExpr>()
            .map_err(|error| format!("invalid cfg predicate {:?}: {error}", gate.predicate))?;
        table.push_str(&format!("({ordinal}, cfg!({predicate})), "));
    }
    Ok(REGION_TEMPLATE
        .replace("__SUPERCOV_MODULE__", module_name)
        .replace("__SUPERCOV_HITS__", &layout.hits.len().to_string())
        .replace(
            "__SUPERCOV_ASSERTIONS__",
            &layout.assertions.len().to_string(),
        )
        .replace(
            "__SUPERCOV_DECISIONS__",
            &layout.decisions.len().to_string(),
        )
        .replace("__SUPERCOV_CFGS__", &layout.cfgs.len().to_string())
        .replace("__SUPERCOV_CFG_TABLE__", &table)
        .replace("__SUPERCOV_TOKEN__", token)
        .replace(
            "__SUPERCOV_REGION_EXPORT__",
            &format!("__supercov_region_{crate_key}"),
        )
        .replace(
            "__SUPERCOV_CLEAR_EXPORT__",
            &format!("__supercov_clear_{crate_key}"),
        ))
}

struct RegionHeader {
    entries_per_decision: usize,
    body_at: usize,
}

/// A region's header, checked against this instrumentation: its token and
/// the size of every table.
fn region_header(
    bytes: &[u8],
    token: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
) -> Result<RegionHeader, RustProbeReadError> {
    const HEADER: usize = 128;
    let u32_at = |offset: usize| -> Result<u32, RustProbeReadError> {
        bytes
            .get(offset..offset + 4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap_or([0; 4])))
            .ok_or(RustProbeReadError::InvalidHeader)
    };
    if bytes.get(..8) != Some(REGION_MAGIC.as_slice())
        || u32_at(8)? != REGION_VERSION
        || u32_at(12)? as usize != HEADER
        || u32_at(16)? as usize != layout.hits.len()
        || u32_at(20)? as usize != layout.assertions.len()
        || u32_at(24)? as usize != layout.decisions.len()
        || u32_at(72)? as usize != layout.cfgs.len()
    {
        return Err(RustProbeReadError::InvalidHeader);
    }
    let stored_token = bytes
        .get(56..56 + token.len())
        .ok_or(RustProbeReadError::InvalidHeader)?;
    if stored_token != token.as_bytes() {
        return Err(RustProbeReadError::InvalidHeader);
    }
    Ok(RegionHeader {
        entries_per_decision: u32_at(28)? as usize,
        body_at: u32_at(32)? as usize,
    })
}

/// One region, read back as the records its process wrote, in the order it
/// wrote them: each slot holds its first sighting's sequence and thread, and
/// the overflow file beside it the decision combinations the region had no
/// room for. Pages the bitmap says no probe touched are not read.
pub fn parse_rust_region(
    bytes: &[u8],
    overflow: Option<&[u8]>,
    token: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
) -> Result<Vec<RustProbeEntry>, RustProbeReadError> {
    const HEADER: usize = 128;
    const PAGE: usize = 4096;
    const ENTRY: usize = 32;
    let u64_at = |offset: usize| -> Option<u64> {
        bytes
            .get(offset..offset + 8)
            .map(|word| u64::from_le_bytes(word.try_into().unwrap_or([0; 8])))
    };
    let RegionHeader {
        entries_per_decision,
        body_at,
    } = region_header(bytes, token, layout)?;
    let touched = |offset: usize| -> bool {
        let page = (offset - body_at) / PAGE;
        bytes
            .get(HEADER + page / 8)
            .is_some_and(|byte| byte & (1 << (page % 8)) != 0)
    };
    let slot = |offset: usize| -> Option<(u64, u64)> {
        if !touched(offset) {
            return None;
        }
        let value = u64_at(offset)?;
        (value != 0).then_some((value >> 32, value & 0xffff_ffff))
    };
    let mut records = Vec::<(u64, RustProbeEntry)>::new();
    let hits_at = body_at;
    for (ordinal, id) in layout.hits.iter().enumerate() {
        if let Some((sequence, thread)) = slot(hits_at + ordinal * 8) {
            records.push((
                sequence,
                RustProbeEntry {
                    thread,
                    observation: RustProbeObservation::Hit { id: id.clone() },
                },
            ));
        }
    }
    let assertions_at = hits_at + layout.hits.len() * 8;
    for (ordinal, id) in layout.assertions.iter().enumerate() {
        if let Some((sequence, thread)) = slot(assertions_at + ordinal * 8) {
            records.push((
                sequence,
                RustProbeEntry {
                    thread,
                    observation: RustProbeObservation::Assertion { id: id.clone() },
                },
            ));
        }
    }
    let decisions_at = assertions_at + layout.assertions.len() * 8;
    let vector = |values: u128, width: usize| -> Vec<Option<bool>> {
        (0..width)
            .map(|index| match (values >> (2 * index)) & 3 {
                1 => Some(false),
                2 => Some(true),
                _ => None,
            })
            .collect()
    };
    for (ordinal, id) in layout.decisions.iter().enumerate() {
        for entry in 0..entries_per_decision {
            let at = decisions_at + (ordinal * entries_per_decision + entry) * ENTRY;
            if !touched(at) || bytes.get(at) != Some(&2) {
                continue;
            }
            let outcome = bytes.get(at + 1).copied().unwrap_or(0) == 2;
            let width = bytes.get(at + 2).copied().unwrap_or(0) as usize;
            let values = bytes
                .get(at + 16..at + 32)
                .map(|word| u128::from_le_bytes(word.try_into().unwrap_or([0; 16])))
                .unwrap_or(0);
            let Some(first) = u64_at(at + 8).filter(|value| *value != 0) else {
                continue;
            };
            records.push((
                first >> 32,
                RustProbeEntry {
                    thread: first & 0xffff_ffff,
                    observation: RustProbeObservation::Decision {
                        id: id.clone(),
                        values: vector(values, width),
                        outcome,
                    },
                },
            ));
        }
    }
    // Overflow: `D <sequence> <thread> <ordinal> <values> <outcome>`.
    for (index, line) in overflow
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let line =
            std::str::from_utf8(line).map_err(|_| RustProbeReadError::InvalidRecord(index + 1))?;
        let fields = line.split('\t').collect::<Vec<_>>();
        let invalid = || RustProbeReadError::InvalidRecord(index + 1);
        let [kind, sequence, thread, ordinal, values, outcome] = fields.as_slice() else {
            // A record cut off by the process dying mid-write.
            continue;
        };
        if *kind != "D" {
            return Err(invalid());
        }
        let id = layout
            .decisions
            .get(ordinal.parse::<usize>().map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        let values = values
            .bytes()
            .map(|digit| match digit {
                b'1' => Ok(Some(false)),
                b'2' => Ok(Some(true)),
                b'0' => Ok(None),
                _ => Err(invalid()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        records.push((
            sequence.parse().map_err(|_| invalid())?,
            RustProbeEntry {
                thread: thread.parse().map_err(|_| invalid())?,
                observation: RustProbeObservation::Decision {
                    id: id.clone(),
                    values,
                    outcome: *outcome == "1",
                },
            },
        ));
    }
    records.sort_by_key(|(sequence, _)| *sequence);
    Ok(records.into_iter().map(|(_, entry)| entry).collect())
}

/// What the builds of a run said of each `cfg` gate: bit 0 some crate was
/// built without the gated code, bit 1 some crate with it. Zero: no process
/// of a crate holding it ever mapped a region, and the run cannot tell.
pub fn parse_rust_region_cfgs(
    bytes: &[u8],
    token: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
) -> Result<Vec<u8>, RustProbeReadError> {
    let RegionHeader {
        entries_per_decision,
        body_at,
    } = region_header(bytes, token, layout)?;
    let cfgs_at = body_at
        + layout.hits.len() * 8
        + layout.assertions.len() * 8
        + layout.decisions.len() * entries_per_decision * 32;
    Ok((0..layout.cfgs.len())
        .map(|ordinal| bytes.get(cfgs_at + ordinal).copied().unwrap_or(0) & 3)
        .collect())
}

/// Every region of `token`'s instrumentation under `root`, at any depth --
/// a run keeps a directory per test -- with what each said of the `cfg`
/// gates, accumulated.
pub fn read_rust_cfg_settlement(
    root: &Path,
    token: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
) -> Result<Vec<u8>, RustProbeReadError> {
    let mut settled = vec![0_u8; layout.cfgs.len()];
    if layout.cfgs.is_empty() {
        return Ok(settled);
    }
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(RustProbeReadError::Io(error.to_string())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| RustProbeReadError::Io(error.to_string()))?;
            let kind = entry
                .file_type()
                .map_err(|error| RustProbeReadError::Io(error.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file()
                && name.starts_with(&format!("{token}-"))
                && name.ends_with(".region")
            {
                let bytes = fs::read(entry.path())
                    .map_err(|error| RustProbeReadError::Io(error.to_string()))?;
                for (slot, bits) in settled
                    .iter_mut()
                    .zip(parse_rust_region_cfgs(&bytes, token, layout)?)
                {
                    *slot |= bits;
                }
            }
        }
    }
    Ok(settled)
}

/// Every region of `token`'s instrumentation in a test's evidence
/// directory -- one per process the test ran -- read back as records, by
/// file name. A region another instrumentation wrote (a fixture instrumented
/// on its own inside the workspace) is not this run's and is left out.
pub fn read_rust_region_directory(
    directory: &Path,
    token: &str,
    layout: &crate::rust_instrumenter::RustProbeLayout,
) -> Result<Vec<(String, Vec<RustProbeEntry>)>, RustProbeReadError> {
    let mut files = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(RustProbeReadError::Io(error.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| RustProbeReadError::Io(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&format!("{token}-")) || !name.ends_with(".region") {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| RustProbeReadError::Io(error.to_string()))?
            .is_file()
        {
            return Err(RustProbeReadError::UnsafeEntry(name));
        }
        files.push(name);
    }
    files.sort();
    let mut read = Vec::new();
    for name in files {
        let bytes = fs::read(directory.join(&name))
            .map_err(|error| RustProbeReadError::Io(error.to_string()))?;
        let overflow_name = format!("{}.overflow", name.trim_end_matches(".region"));
        let overflow = match fs::read(directory.join(&overflow_name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(RustProbeReadError::Io(error.to_string())),
        };
        read.push((
            name,
            parse_rust_region(&bytes, overflow.as_deref(), token, layout)?,
        ));
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::rust_instrumenter::{RustProbeLayout, instrument_rust_source};
    use std::collections::BTreeMap;

    const TOKEN: &str = "0123456789ab";

    /// The region runtime as a crate root gets it, for a source whose probes
    /// were numbered alone.
    fn region_runtime(probes: &RustProbeLayout) -> String {
        // One crate: every gate is in its files.
        let crate_cfgs = (0..probes.cfgs.len()).collect::<Vec<_>>();
        render_rust_region_runtime(
            "__supercov_runtime_v1",
            TOKEN,
            "abcdef012345",
            probes,
            &crate_cfgs,
        )
        .unwrap()
    }

    /// Every region an evidence directory holds, read back as records.
    fn read_regions(
        evidence: &std::path::Path,
        probes: &RustProbeLayout,
    ) -> std::collections::BTreeMap<String, Vec<RustProbeEntry>> {
        read_rust_region_directory(evidence, TOKEN, probes)
            .unwrap()
            .into_iter()
            .collect()
    }

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-rust-runtime-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    /// A program in a container writes into a region on a bind mount the
    /// host reads while it runs. Docker Desktop's VM shows the host a shared
    /// mapping only after `msync`, so with `SUPERCOV_CONTAINER` set a flusher
    /// syncs every few milliseconds and stamps an epoch. Needs Docker and
    /// pulls an image: `scripts/rust-region-container.mjs` runs it.
    #[test]
    #[ignore = "needs a Docker daemon; run by scripts/rust-region-container.mjs"]
    fn a_region_in_a_container_is_read_live_from_the_host() {
        // The program maps its region first (main's own probe), waits to be
        // told to go, takes the decision, and says so: the host then has a
        // second to see it while the program sleeps on. Without the flusher
        // the VM's writeback shows it only seconds later, or at exit.
        let source = r#"fn work(n: u32) -> u32 {
    if n > 2 { n * 2 } else { n }
}

fn main() {
    std::fs::write("/w/ready", "ready").unwrap();
    while !std::path::Path::new("/w/go").exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    println!("{}", work(5));
    std::fs::write("/w/done", "done").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("container");
        fs::write(
            directory.join("main.rs"),
            format!("{}\n{runtime}", transformed.code),
        )
        .unwrap();
        let image =
            std::env::var("SUPERCOV_RUST_IMAGE").unwrap_or_else(|_| "rust:1-bookworm".into());
        let name = format!(
            "supercov-region-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let started = Command::new("docker")
            .args(["run", "-d", "--name", &name])
            .arg("-v")
            .arg(format!("{}:/w", directory.display()))
            .args([
                "-e",
                "SUPERCOV_CONTAINER=1",
                "-e",
                "SUPERCOV_RUST_EVIDENCE_DIR=/w/evidence",
                &image,
                "sh",
                "-c",
                "cd /tmp && rustc --edition=2024 -O /w/main.rs -o program && ./program",
            ])
            .output()
            .unwrap();
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        struct Removed(String);
        impl Drop for Removed {
            fn drop(&mut self) {
                let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
            }
        }
        let _container = Removed(name.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        while !directory.join("ready").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the program never got ready: {}",
                String::from_utf8_lossy(
                    &Command::new("docker")
                        .args(["logs", &name])
                        .output()
                        .unwrap()
                        .stderr
                )
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Let the guest's periodic writeback carry everything written so far.
        std::thread::sleep(std::time::Duration::from_secs(6));
        fs::write(directory.join("go"), "go").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !directory.join("done").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the program never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The program sleeps for two minutes yet: whatever the host reads
        // now came through the live mapping.
        let evidence = directory.join("evidence");
        let work = transformed
            .manifest
            .decisions
            .iter()
            .find(|decision| decision.line == 2)
            .unwrap()
            .id
            .clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let (decisions, epoch) = loop {
            let regions = read_regions(&evidence, &transformed.probes);
            let decisions = regions
                .values()
                .flatten()
                .filter(|entry| {
                    matches!(&entry.observation, RustProbeObservation::Decision { id, .. } if *id == work)
                })
                .count();
            let epoch = fs::read_dir(&evidence)
                .ok()
                .and_then(|mut entries| entries.next())
                .and_then(|entry| fs::read(entry.ok()?.path()).ok())
                .and_then(|bytes| {
                    bytes
                        .get(48..52)
                        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                })
                .unwrap_or(0);
            if decisions > 0 && epoch > 0 || std::time::Instant::now() > deadline {
                break (decisions, epoch);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let running = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", &name])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&running.stdout).trim(), "true");
        assert_eq!(
            decisions, 1,
            "the decision the program took is visible while it runs"
        );
        assert!(epoch > 0, "the flusher stamps its epoch");
        drop(_container);
        let _ = Command::new("docker")
            .args(["run", "--rm", "-v"])
            .arg(format!("{}:/w", directory.display()))
            .args([
                &image,
                "rm",
                "-rf",
                "/w/evidence",
                "/w/ready",
                "/w/go",
                "/w/done",
            ])
            .output();
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn the_runtime_records_which_gated_code_the_compiler_built() {
        let source = r#"#[cfg(feature = "on")]
fn built() -> u8 {
    if std::env::args().count() > 99 { 1 } else { 2 }
}

#[cfg(feature = "off")]
fn left_out() -> u8 {
    3
}

#[cfg(not(feature = "on"))]
fn not_on() -> u8 {
    4
}

fn main() {
    println!("{}", built());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let predicates = transformed
            .probes
            .cfgs
            .iter()
            .map(|gate| gate.predicate.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            predicates,
            [
                "feature = \"on\"",
                "feature = \"off\"",
                "not(feature = \"on\")"
            ]
        );
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("cfg-table");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence").join("test-1");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .args(["--edition=2024", "--cfg", "feature=\"on\""])
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"2\n");
        // Read from the run's root, a directory per test below it.
        let settled =
            read_rust_cfg_settlement(&directory.join("evidence"), TOKEN, &transformed.probes)
                .unwrap();
        assert_eq!(settled, [2, 1, 1]);
        // A process that never maps a region answers for nothing.
        let silent = directory.join("silent");
        fs::create_dir(&silent).unwrap();
        assert_eq!(
            read_rust_cfg_settlement(&silent, TOKEN, &transformed.probes).unwrap(),
            [0, 0, 0]
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_runtime_records_owned_points_and_exact_short_circuit_vectors() {
        let source = r#"fn choose(first: bool, second: bool) -> i32 {
    if first && second { 7 } else { 3 }
}

fn main() {
    println!("{} {}", choose(false, true), choose(true, true));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("record");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"3 7\n");
        let files = read_regions(&evidence, &transformed.probes);
        assert_eq!(files.len(), 1);
        let observations = files.values().next().unwrap();
        let decisions = observations
            .iter()
            .filter_map(|entry| match &entry.observation {
                RustProbeObservation::Decision {
                    values, outcome, ..
                } => Some((values.clone(), *outcome)),
                RustProbeObservation::Hit { .. } | RustProbeObservation::Assertion { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decisions,
            [
                (vec![Some(false), None], false),
                (vec![Some(true), Some(true)], true)
            ]
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_runtime_compiles_into_a_no_std_host_crate() {
        // `#![no_std]` swaps the std prelude for core's, so `format!`, `vec!`
        // and `Vec` are simply not in scope. The injected module named them
        // unqualified and every no_std crate failed to build -- found on
        // bytes-1.12.1, which is `#![no_std]` (src/lib.rs:6). `extern crate std`
        // here mirrors what the injected module does: it links std without
        // restoring the prelude, which is precisely the condition under test.
        let source = r#"#![no_std]

extern crate std;

fn choose(first: bool, second: bool) -> i32 {
    if first && second { 7 } else { 3 }
}

fn main() {
    std::println!("{} {}", choose(false, true), choose(true, true));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("no-std");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"3 7\n");
        // Probes must still record, not merely compile.
        let files = read_regions(&evidence, &transformed.probes);
        assert_eq!(files.len(), 1);
        assert!(!files.values().next().unwrap().is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn probes_reached_through_a_global_allocator_do_not_recurse() {
        // The shape from bytes-1.12.1 tests/test_bytes_vec_alloc.rs: the
        // allocator's `alloc` calls an inherent method, which calls a FREE
        // FUNCTION. Skipping `impl GlobalAlloc` blocks syntactically does not
        // cover `note`, and nothing syntactic can -- the chain may leave the
        // file or the crate. Only the runtime knows a probe is already running,
        // so the guard has to live there. Without it this binary dies with
        // SIGSEGV instead of printing anything.
        let source = r#"use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEEN: AtomicUsize = AtomicUsize::new(0);

fn note(size: usize) {
    if size > 0 {
        SEEN.fetch_add(1, Ordering::SeqCst);
    }
}

struct Ledger;

impl Ledger {
    fn record(&self, size: usize) {
        note(size);
    }
}

unsafe impl GlobalAlloc for Ledger {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.record(layout.size());
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // dealloc must be instrumented too, or the test never exercises the
        // ladder that actually crashed: freeing the probe's OWN buffer re-enters
        // here, and a guard released before that free recurses without bound.
        self.record(layout.size());
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static LEDGER: Ledger = Ledger;

fn classify(flag: bool) -> usize {
    if flag { 1 } else { 2 }
}

fn main() {
    let held = std::vec![7u8; 32];
    println!("{} {}", classify(!held.is_empty()), held.len());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // `note` is a free function, so it IS instrumented -- proving the guard,
        // not a syntactic skip, is what prevents the recursion.
        assert!(
            transformed
                .code
                .contains("fn note(size: usize) {crate::__supercov_runtime_v1::hit("),
            "the free function reached from the allocator should still be probed"
        );
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("allocator-reentry");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "instrumented allocator did not survive: {:?}",
            output.status
        );
        assert_eq!(output.stdout, b"1 32\n");
        let files = read_regions(&evidence, &transformed.probes);
        let observations = files.values().next().unwrap();
        let decisions = observations
            .iter()
            .filter_map(|entry| match &entry.observation {
                RustProbeObservation::Decision {
                    id,
                    values,
                    outcome,
                } => Some((id.clone(), values.clone(), *outcome)),
                RustProbeObservation::Hit { .. } | RustProbeObservation::Assertion { .. } => None,
            })
            .collect::<Vec<_>>();
        // Suppression costs only duplicates: `classify` runs outside any probe,
        // so its decision is still recorded exactly.
        let classify = transformed
            .manifest
            .decisions
            .iter()
            .find(|decision| decision.source == "flag")
            .expect("classify's decision reached the manifest");
        assert!(
            decisions
                .iter()
                .any(|(id, values, outcome)| id == &classify.id
                    && values == &[Some(true)]
                    && *outcome),
            "classify's decision was lost: {decisions:?}"
        );
        // `note` records too -- every ordinary allocation reaches it outside a
        // probe -- which is why suppressing the nested ones loses nothing.
        let note = transformed
            .manifest
            .decisions
            .iter()
            .find(|decision| decision.source == "size > 0")
            .expect("note's decision reached the manifest");
        assert!(decisions.iter().any(|(id, ..)| id == &note.id));
        // No frame built while nested may reach the log: a zero-width vector
        // for a one-condition decision is a malformed record, not a lost one.
        assert!(
            decisions.iter().all(|(_, values, _)| values.len() == 1),
            "a suppressed frame emitted a malformed vector: {decisions:?}"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_hit_in_a_loop_is_written_once_but_decisions_keep_every_vector() {
        // bytes' advance_bytes_mut_remaining_capacity is a triple-nested loop of
        // ~2.8M iterations. One write syscall per probe per iteration left it
        // still running after four minutes. A hit only answers "did this ever
        // run", so the repeats carry nothing -- but a decision's condition
        // vector differs per iteration and every distinct one must survive.
        let source = r#"fn step(value: usize) -> bool {
    let doubled = value * 2;
    doubled > 4
}

fn main() {
    let mut seen = 0;
    for value in 0..64 {
        if step(value) { seen += 1; }
    }
    println!("{seen}");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("dedup");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"61\n");
        let files = read_regions(&evidence, &transformed.probes);
        let observations = files.values().next().unwrap();

        // `doubled > 4` runs 64 times; its hit is recorded once.
        let mut hits = BTreeMap::<&str, usize>::new();
        for entry in observations {
            if let RustProbeObservation::Hit { id } = &entry.observation {
                *hits.entry(id.as_str()).or_default() += 1;
            }
        }
        assert!(!hits.is_empty(), "no hits recorded at all");
        assert!(
            hits.values().all(|count| *count == 1),
            "a repeated hit was written more than once: {hits:?}"
        );

        // MC/DC needs the SET of condition vectors, not how often each recurred,
        // so a repeat of an already-seen vector carries nothing -- but every
        // DISTINCT vector must still arrive. `doubled > 4` is evaluated 64 times
        // and takes exactly two distinct shapes, so exactly two records survive.
        let decisions = observations
            .iter()
            .filter_map(|entry| match &entry.observation {
                RustProbeObservation::Decision {
                    values, outcome, ..
                } => Some((values.clone(), *outcome)),
                RustProbeObservation::Hit { .. } | RustProbeObservation::Assertion { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decisions,
            // In first-occurrence order: `step(0)` is false before any value
            // exceeds the threshold.
            [(vec![Some(false)], false), (vec![Some(true)], true)],
            "both distinct vectors must survive, and neither may repeat"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_runtime_survives_a_host_without_the_prelude() {
        // tracing's macro tests carry `#![no_implicit_prelude]` on purpose,
        // to prove their macros do not depend on it. The runtime imports
        // every name it uses for `no_std` hosts, but three `TryProbe` impls
        // reached for `matches!`, which is a prelude macro.
        let source = r#"#![no_implicit_prelude]

fn parse(text: &str) -> ::std::option::Option<i32> {
    let value: i32 = text.parse().ok()?;
    ::std::option::Option::Some(value * 2)
}

fn main() {
    ::std::println!("{:?} {:?}", parse("4"), parse("x"));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // The `?` takes a probe, which is what pulls in the TryProbe impls.
        assert!(transformed.code.contains("TryProbe"));
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("no-implicit-prelude");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--cap-lints=allow")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary).output().unwrap();
        assert_eq!(output.stdout, b"Some(8) None\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_runtime_survives_a_deny_warnings_host() {
        // serde builds with `#![deny(warnings)]`; the injected module's
        // fully-qualified imports (required for no_std hosts) read as unused
        // imports and became hard errors. Injected code must be immune to the
        // host's lint policy.
        let source = r#"#![deny(warnings)]

fn choose(first: bool, second: bool) -> i32 {
    if first && second { 7 } else { 3 }
}

fn main() {
    println!("{} {}", choose(false, true), choose(true, true));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let runtime = region_runtime(&transformed.probes);
        let directory = temporary_directory("deny-warnings");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        fs::write(&input, format!("{}\n{runtime}", transformed.code)).unwrap();
        // --cap-lints=allow mirrors what the runner passes for the instrumented
        // workspace: the host policy must not reject generated code, including
        // the `if ({{ frame ... }})` decision wrapping that trips unused_parens.
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--cap-lints=allow")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary).output().unwrap();
        assert_eq!(output.stdout, b"3 7\n");
        fs::remove_dir_all(directory).unwrap();
    }

    fn layout(hits: &[&str], decisions: &[&str]) -> RustProbeLayout {
        RustProbeLayout {
            token: TOKEN.into(),
            hits: hits.iter().map(|id| (*id).to_owned()).collect(),
            assertions: Vec::new(),
            decisions: decisions.iter().map(|id| (*id).to_owned()).collect(),
            cfgs: Vec::new(),
        }
    }

    #[test]
    fn a_region_of_another_layout_or_instrumentation_is_refused() {
        let probes = layout(&["rs:point:a"], &[]);
        assert_eq!(
            parse_rust_region(b"not a region", None, TOKEN, &probes),
            Err(RustProbeReadError::InvalidHeader)
        );
        // A valid header, written for two probes, read with a layout of one.
        let mut bytes = vec![0u8; 4096];
        bytes[..8].copy_from_slice(b"SCVRGN01");
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&128u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&2u32.to_le_bytes());
        bytes[56..56 + TOKEN.len()].copy_from_slice(TOKEN.as_bytes());
        assert_eq!(
            parse_rust_region(&bytes, None, TOKEN, &probes),
            Err(RustProbeReadError::InvalidHeader)
        );
        // The right layout, but another instrumentation's token.
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            parse_rust_region(&bytes, None, "ba9876543210", &probes),
            Err(RustProbeReadError::InvalidHeader)
        );
        assert_eq!(
            parse_rust_region(&bytes, None, TOKEN, &probes),
            Ok(Vec::new())
        );
    }

    #[test]
    fn the_directory_reader_takes_only_this_instrumentations_regular_files() {
        let directory = temporary_directory("regions");
        let probes = layout(&["rs:point:a"], &[]);
        fs::write(
            directory.join("ba9876543210-7.region"),
            b"another instrumentation",
        )
        .unwrap();
        fs::write(directory.join("notes.txt"), b"unrelated").unwrap();
        assert_eq!(
            read_rust_region_directory(&directory, TOKEN, &probes),
            Ok(Vec::new())
        );
        fs::create_dir(directory.join(format!("{TOKEN}-9.region"))).unwrap();
        assert!(matches!(
            read_rust_region_directory(&directory, TOKEN, &probes),
            Err(RustProbeReadError::UnsafeEntry(_))
        ));
        assert!(
            read_rust_region_directory(&directory.join("absent"), TOKEN, &probes)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn combinations_beyond_a_decisions_entries_overflow_and_are_all_read_back() {
        // `(a || b) && (c || d) && e` reaches eleven distinct (vector,
        // outcome) combinations, against eight entries in the region.
        let source = r#"fn all(a: bool, b: bool, c: bool, d: bool, e: bool) -> bool {
    if (a || b) && (c || d) && e { true } else { false }
}

fn main() {
    let mut taken = 0;
    for bits in 0..32u8 {
        for _ in 0..3 {
            if all(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0, bits & 16 != 0) {
                taken += 1;
            }
        }
    }
    println!("{taken}");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let directory = temporary_directory("overflow");
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        let evidence = directory.join("evidence");
        fs::write(
            &input,
            format!(
                "{}\n{}",
                transformed.code,
                region_runtime(&transformed.probes)
            ),
        )
        .unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary)
            .env("SUPERCOV_RUST_EVIDENCE_DIR", &evidence)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"27\n");
        let mut vectors = read_regions(&evidence, &transformed.probes)
            .into_values()
            .flatten()
            .filter_map(|entry| match entry.observation {
                // The five-condition decision; `main`'s `if` is one of its own.
                RustProbeObservation::Decision {
                    values, outcome, ..
                } if values.len() == 5 => Some((values, outcome)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let total = vectors.len();
        vectors.sort();
        vectors.dedup();
        // Every combination, the three the region had no room for included,
        // read back once.
        assert_eq!(total, vectors.len(), "a combination was recorded twice");
        assert_eq!(vectors.len(), 11, "{vectors:?}");
        // A record cut off by the process dying mid-write is not an error.
        let overflow = fs::read_dir(&evidence)
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".overflow"));
        let overflow = overflow.expect("the combinations past eight overflowed");
        {
            let mut bytes = fs::read(overflow.path()).unwrap();
            bytes.extend_from_slice(b"D\t99\t1");
            fs::write(overflow.path(), bytes).unwrap();
            assert!(read_rust_region_directory(&evidence, TOKEN, &transformed.probes).is_ok());
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
