//! Ids that name the same thing the same way in every run: the first twelve
//! bytes, in hex, of a SHA-256 of their parts, each ended by a zero byte.

use sha2::{Digest, Sha256};

/// The hex digest of `parts`.
pub(crate) fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    parts.iter().for_each(|part| {
        hash.update(part.as_bytes());
        hash.update([0]);
    });
    hash.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `prefix:` and the digest of `parts`.
pub(crate) fn prefixed(prefix: &str, parts: &[&str]) -> String {
    format!("{prefix}:{}", digest(parts))
}
