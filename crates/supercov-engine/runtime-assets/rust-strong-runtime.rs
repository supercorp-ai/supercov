#[doc(hidden)]
// Injected code must be immune to the host crate's lint configuration, and
// work in a `#![no_std]` crate or one without the prelude.
#[allow(warnings)]
mod __SUPERCOV_MODULE__ {
    extern crate std;
    use std::option::Option::{None, Some};
    use std::result::Result::Ok;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The active mutant's ordinal; `UNREAD` before the environment is read,
    /// `NONE` where it names none.
    static ACTIVE: AtomicU64 = AtomicU64::new(UNREAD);
    static HITS: AtomicU64 = AtomicU64::new(0);
    static LIMIT: AtomicU64 = AtomicU64::new(0);
    const UNREAD: u64 = u64::MAX;
    const NONE: u64 = u64::MAX - 1;

    #[cold]
    #[inline(never)]
    fn read() -> u64 {
        let number = |name: &str| match std::env::var(name) {
            Ok(value) => value.trim().parse::<u64>().ok(),
            _ => None,
        };
        LIMIT.store(number("SUPERCOV_MUTANT_HIT_LIMIT").unwrap_or(0), Ordering::Relaxed);
        let active = number("SUPERCOV_ACTIVE_MUTANT").unwrap_or(NONE);
        ACTIVE.store(active, Ordering::Relaxed);
        active
    }

    #[cold]
    #[inline(never)]
    fn timeout() -> ! {
        // Past the limit a loop the mutant made endless ends here, in its own
        // process; nothing the code under test catches can carry on.
        let _ = std::io::Write::write_all(&mut std::io::stderr(), b"\nSUPERCOV_MUTANT_TIMEOUT\n");
        std::process::abort()
    }

    /// Whether mutant `ordinal` is the process's active one. Counts its
    /// evaluations.
    #[inline]
    pub fn on(ordinal: u64) -> bool {
        let mut active = ACTIVE.load(Ordering::Relaxed);
        if active == UNREAD {
            active = read();
        }
        if active != ordinal {
            return false;
        }
        let hits = HITS.fetch_add(1, Ordering::Relaxed) + 1;
        let limit = LIMIT.load(Ordering::Relaxed);
        if limit != 0 && hits > limit {
            timeout();
        }
        true
    }
}
