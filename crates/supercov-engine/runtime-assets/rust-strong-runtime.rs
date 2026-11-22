#[doc(hidden)]
// Injected code must be immune to the host crate's lint configuration, and
// work in a `#![no_std]` crate or one without the prelude.
#[allow(warnings)]
mod __SUPERCOV_MODULE__ {
    extern crate std;
    use std::option::Option::{self, None, Some};
    use std::result::Result::Ok;
    use std::string::String;
    use std::borrow::ToOwned;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The active mutant's index in its file's list; `UNREAD` before the
    /// environment is read, `NONE` where it names none.
    static ACTIVE: AtomicU64 = AtomicU64::new(UNREAD);
    /// The active mutant's file, as `mutants.json` keys it.
    static FILE: OnceLock<String> = OnceLock::new();
    static HITS: AtomicU64 = AtomicU64::new(0);
    static LIMIT: AtomicU64 = AtomicU64::new(0);
    const UNREAD: u64 = u64::MAX;
    const NONE: u64 = u64::MAX - 1;
    /// The hit limit where `SUPERCOV_MUTANT_HIT_LIMIT` is unset; `0` there
    /// is none.
    const DEFAULT_LIMIT: u64 = 1_000_000;

    /// Reads `SUPERCOV_ACTIVE_MUTANT` (`<file>#<index>`, `-` for none).
    #[cold]
    #[inline(never)]
    fn read() -> u64 {
        let limit = match std::env::var("SUPERCOV_MUTANT_HIT_LIMIT") {
            Ok(value) => value.trim().parse::<u64>().unwrap_or(DEFAULT_LIMIT),
            _ => DEFAULT_LIMIT,
        };
        LIMIT.store(limit, Ordering::Relaxed);
        let named = match std::env::var("SUPERCOV_ACTIVE_MUTANT") {
            Ok(value) => value,
            _ => String::new(),
        };
        let active = match named.rsplit_once('#') {
            Some((file, index)) => match index.parse::<u64>() {
                Ok(index) if index < NONE => {
                    let _ = FILE.set(String::from(file));
                    index
                }
                _ => NONE,
            },
            None => NONE,
        };
        ACTIVE.store(active, Ordering::Relaxed);
        active
    }

    /// Past the limit a loop the mutant made endless ends here, in its own
    /// process, which nothing the code under test catches can carry on
    /// from. The pass reads the line it appends beside the results.
    #[cold]
    #[inline(never)]
    fn timeout(file: &str, index: u64) -> ! {
        if let Ok(results) = std::env::var("SUPERCOV_STRONG_RESULTS") {
            let line = std::format!(
                "{{\"mutant\": {:?}, \"attemptId\": null, \"pass\": \"main\"}}\n",
                std::format!("{file}#{index}")
            );
            if let Ok(mut out) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::format!("{results}.timeouts.jsonl"))
            {
                let _ = std::io::Write::write_all(&mut out, line.as_bytes());
            }
        }
        let _ = std::io::Write::write_all(&mut std::io::stderr(), b"\nSUPERCOV_MUTANT_TIMEOUT\n");
        std::process::abort()
    }

    // A wasm module has no environment: the host that instantiates it (the
    // Node hook of `rust-wasm-node.mjs`) writes the active mutant's file
    // into this buffer and names its index, before any test runs.
    #[cfg(target_arch = "wasm32")]
    static mut BUFFER: [u8; 4096] = [0; 4096];

    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn __SUPERCOV_BUFFER_EXPORT__() -> u32 {
        unsafe { (&raw mut BUFFER) as usize as u32 }
    }

    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn __SUPERCOV_ACTIVATE_EXPORT__(length: u32, index: u64) {
        let length = (length as usize).min(4096);
        let bytes = unsafe { &(*(&raw const BUFFER))[..length] };
        let _ = FILE.set(String::from_utf8_lossy(bytes).into_owned());
        LIMIT.store(0, Ordering::Relaxed);
        ACTIVE.store(if index < NONE { index } else { NONE }, Ordering::Relaxed);
    }

    /// Whether mutant `index` of `file` is the process's active one. Counts
    /// its evaluations.
    #[inline]
    pub fn on(file: &'static str, index: u64) -> bool {
        let mut active = ACTIVE.load(Ordering::Relaxed);
        if active == UNREAD {
            active = read();
        }
        if active != index || FILE.get().map(String::as_str) != Some(file) {
            return false;
        }
        let hits = HITS.fetch_add(1, Ordering::Relaxed) + 1;
        let limit = LIMIT.load(Ordering::Relaxed);
        if limit != 0 && hits > limit {
            timeout(file, index);
        }
        true
    }
}
