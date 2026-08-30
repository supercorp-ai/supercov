//! Generated std-only runtime and strict reader for owned Rust probes.
//!
//! The target runtime writes an intentionally small append-only transport.
//! It never computes coverage. Rust reads, validates, de-duplicates and maps
//! these observations into the shared evidence-v3 model after test execution.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
};

const RUST_PROBE_MAGIC: &str = "SUPERCOV-RUST-PROBE-1";

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

pub(crate) fn valid_probe_id(id: &str) -> bool {
    let mut parts = id.split(':');
    matches!(parts.next(), Some("rs"))
        && matches!(
            parts.next(),
            Some("statement" | "function" | "decision" | "branch")
        )
        && parts.next().is_some_and(|digest| {
            digest.len() == 24 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && parts.all(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

pub fn render_rust_runtime(module_name: &str, crate_key: &str) -> Result<String, String> {
    let valid_identifier = !module_name.is_empty()
        && module_name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
    if !valid_identifier {
        return Err("invalid Rust runtime module name".into());
    }
    if crate_key.len() != 24 || !crate_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Rust runtime crate key".into());
    }

    Ok(format!(
        r#"
#[doc(hidden)]
#[allow(dead_code)]
mod {module_name} {{
    // The host crate may be `#![no_std]` -- `bytes` is, and so is much of the
    // ecosystem's foundation. Nothing here can rely on the std prelude being in
    // scope, so std is brought in explicitly and every prelude item below is
    // written out in full. Without this the module does not compile and the
    // whole build fails, which is a hard failure rather than a degradation.
    extern crate std;
    use std::fs::{{File, OpenOptions}};
    use std::io::Write as _;
    use std::option::Option::{{self, None, Some}};
    use std::string::String;
    use std::sync::{{Mutex, OnceLock}};
    use std::vec::Vec;

    const MAGIC: &[u8] = b"{RUST_PROBE_MAGIC}\n";
    const CRATE_KEY: &str = "{crate_key}";

    fn writer() -> Option<&'static Mutex<File>> {{
        static WRITER: OnceLock<Option<Mutex<File>>> = OnceLock::new();
        WRITER.get_or_init(|| {{
            let directory = std::env::var_os("SUPERCOV_RUST_EVIDENCE_DIR")?;
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).ok()?;
            let path =
                directory.join(std::format!("{{CRATE_KEY}}-{{}}.events", std::process::id()));
            let empty = std::fs::metadata(&path).map_or(true, |metadata| metadata.len() == 0);
            let mut file = OpenOptions::new().create(true).append(true).open(path).ok()?;
            if empty {{
                file.write_all(MAGIC).ok()?;
            }}
            Some(Mutex::new(file))
        }}).as_ref()
    }}

    fn write_record(record: &[u8]) {{
        let Some(writer) = writer() else {{ return }};
        let Ok(mut writer) = writer.lock() else {{ return }};
        let _ = writer.write_all(record);
    }}

    pub struct DecisionFrame {{
        id: &'static str,
        values: Vec<u8>,
    }}

    impl DecisionFrame {{
        pub fn new(id: &'static str, conditions: usize) -> Self {{
            Self {{ id, values: std::vec![0; conditions] }}
        }}
    }}

    #[inline]
    pub fn hit(id: &'static str) {{
        let mut record = Vec::with_capacity(id.len() + 3);
        record.extend_from_slice(b"H\t");
        record.extend_from_slice(id.as_bytes());
        record.push(b'\n');
        write_record(&record);
    }}

    #[inline]
    pub fn condition(value: bool, frame: &mut DecisionFrame, index: usize) -> bool {{
        if let Some(slot) = frame.values.get_mut(index) {{
            *slot = if value {{ 2 }} else {{ 1 }};
        }}
        value
    }}

    #[inline]
    pub fn decision(value: bool, frame: &mut DecisionFrame) -> bool {{
        let mut record = Vec::with_capacity(frame.id.len() + frame.values.len() + 6);
        record.extend_from_slice(b"D\t");
        record.extend_from_slice(frame.id.as_bytes());
        record.push(b'\t');
        for digit in &frame.values {{
            record.push(b'0' + *digit);
        }}
        record.push(b'\t');
        record.push(if value {{ b'1' }} else {{ b'0' }});
        record.push(b'\n');
        write_record(&record);
        value
    }}
}}
"#
    ))
}

pub fn parse_rust_probe_events(
    input: &[u8],
) -> Result<Vec<RustProbeObservation>, RustProbeReadError> {
    let text = std::str::from_utf8(input).map_err(|_| RustProbeReadError::InvalidHeader)?;
    let mut lines = text.lines();
    if lines.next() != Some(RUST_PROBE_MAGIC) {
        return Err(RustProbeReadError::InvalidHeader);
    }
    let mut observations = Vec::new();
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        let fields = line.split('\t').collect::<Vec<_>>();
        match fields.as_slice() {
            ["H", id] if valid_probe_id(id) => {
                observations.push(RustProbeObservation::Hit { id: (*id).into() })
            }
            ["D", id, digits, outcome]
                if valid_probe_id(id)
                    && id.starts_with("rs:decision:")
                    && !digits.is_empty()
                    && digits
                        .bytes()
                        .all(|digit| matches!(digit, b'0' | b'1' | b'2'))
                    && matches!(*outcome, "0" | "1") =>
            {
                observations.push(RustProbeObservation::Decision {
                    id: (*id).into(),
                    values: digits
                        .bytes()
                        .map(|digit| match digit {
                            b'0' => None,
                            b'1' => Some(false),
                            b'2' => Some(true),
                            _ => unreachable!(),
                        })
                        .collect(),
                    outcome: *outcome == "1",
                });
            }
            _ => return Err(RustProbeReadError::InvalidRecord(line_number)),
        }
    }
    Ok(observations)
}

pub fn read_rust_probe_directory(
    directory: &Path,
) -> Result<BTreeMap<String, Vec<RustProbeObservation>>, RustProbeReadError> {
    let mut files = fs::read_dir(directory)
        .map_err(|error| RustProbeReadError::Io(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RustProbeReadError::Io(error.to_string()))?;
    files.sort_by_key(|entry| entry.file_name());
    let mut observations = BTreeMap::new();
    for entry in files {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RustProbeReadError::UnsafeEntry("<non-utf8>".into()))?;
        if Path::new(&name)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || !name.ends_with(".events")
        {
            return Err(RustProbeReadError::UnsafeEntry(name));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| RustProbeReadError::Io(error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(RustProbeReadError::UnsafeEntry(name));
        }
        let contents =
            fs::read(entry.path()).map_err(|error| RustProbeReadError::Io(error.to_string()))?;
        observations.insert(name, parse_rust_probe_events(&contents)?);
    }
    Ok(observations)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::rust_instrumenter::instrument_rust_source;

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
        let runtime =
            render_rust_runtime("__supercov_runtime_v1", "0123456789abcdef01234567").unwrap();
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
        let files = read_rust_probe_directory(&evidence).unwrap();
        assert_eq!(files.len(), 1);
        let observations = files.values().next().unwrap();
        let decisions = observations
            .iter()
            .filter_map(|observation| match observation {
                RustProbeObservation::Decision {
                    values, outcome, ..
                } => Some((values.clone(), *outcome)),
                RustProbeObservation::Hit { .. } => None,
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
    fn reader_rejects_truncation_invalid_digits_and_non_files() {
        assert_eq!(
            parse_rust_probe_events(
                b"SUPERCOV-RUST-PROBE-1\nD\trs:decision:0123456789abcdef01234567\t03\t1\n"
            ),
            Err(RustProbeReadError::InvalidRecord(2))
        );
        assert_eq!(
            parse_rust_probe_events(b"SUPERCOV-RUST-PROBE-"),
            Err(RustProbeReadError::InvalidHeader)
        );

        let directory = temporary_directory("unsafe");
        fs::create_dir(directory.join("nested.events")).unwrap();
        assert!(matches!(
            read_rust_probe_directory(&directory),
            Err(RustProbeReadError::UnsafeEntry(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
