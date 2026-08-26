//! Compiler bridge fixture.
//!
//! ```
//! assert_eq!(supercov_rustc_spike_fixture::authored(true), 1);
//! ```

macro_rules! generated_function {
    () => {
        pub fn generated_by_rules(value: bool) -> usize {
            if value { 3 } else { 5 }
        }
    };
}

generated_function!();
probe_macros::generated_function!();

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

pub const fn const_decision(value: bool) -> usize {
    if value { 11 } else { 13 }
}

pub const CONST_VALUE: usize = const_decision(true);

pub fn authored(value: bool) -> usize {
    if value { 1 } else { 2 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exercises_every_surface() {
        assert_eq!(authored(true), 1);
        assert_eq!(generated_by_rules(false), 5);
        assert_eq!(generated_by_proc(true), 17);
        assert_eq!(generated_by_build_script(false), 9);
        assert_eq!(CONST_VALUE, 11);
    }
}
