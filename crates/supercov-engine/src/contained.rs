//! Work on one file that may not take the run down with it.
//!
//! The instrumenters hold invariants a parser and an earlier pass promise:
//! every operand a planned rewrite reaches was planned, every guard the
//! grammar requires is there. A broken one is a bug in Supercov, and one
//! file's bug should cost that file its measurement, said in the run, rather
//! than the run. So a file's work runs here, and a panic in it comes back as
//! an error carrying its message.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// `work`'s value, or the message it panicked with.
pub fn contained<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(work)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "an internal error".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_comes_back_with_its_message() {
        assert_eq!(contained(|| 2), Ok(2));
        assert_eq!(
            contained(|| -> () { panic!("a static message") }),
            Err("a static message".to_owned())
        );
        let detail = 7;
        assert_eq!(
            contained(|| -> () { panic!("formatted {detail}") }),
            Err("formatted 7".to_owned())
        );
        assert_eq!(
            contained(|| -> () { std::panic::panic_any(42) }),
            Err("an internal error".to_owned())
        );
    }
}
