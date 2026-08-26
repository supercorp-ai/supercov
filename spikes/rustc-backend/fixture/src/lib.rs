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
pub const CONST_FALSE_VALUE: usize = const_decision(false);

/// A non-mergeable doctest with a hidden setup line.
///
/// ```standalone_crate
/// # let hidden = std::hint::black_box(20);
/// assert_eq!(hidden + 2, 22);
/// ```
pub fn standalone_doctest_surface() {}

/// The rustdoc launcher must not leak its private unstable-option bootstrap
/// into user doctest compilation.
///
/// ```compile_fail
/// #![feature(test)]
/// extern crate test;
/// ```
pub fn stable_feature_gate_doctest_surface() {}

pub fn authored(value: bool) -> usize {
    if value { 1 } else { 2 }
}

pub fn fallible(value: usize) -> Result<usize, &'static str> {
    if value == 0 {
        Err("zero")
    } else {
        Ok(value + 1)
    }
}

struct DropMarker<'a> {
    label: &'static str,
    log: &'a std::cell::RefCell<Vec<&'static str>>,
}

impl Drop for DropMarker<'_> {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.label);
    }
}

pub fn drop_order(log: &std::cell::RefCell<Vec<&'static str>>) -> usize {
    let _first = DropMarker {
        label: "first",
        log,
    };
    let _second = DropMarker {
        label: "second",
        log,
    };
    23
}

pub fn panic_path(log: &std::cell::RefCell<Vec<&'static str>>) {
    let _guard = DropMarker {
        label: "panic-drop",
        log,
    };
    panic!("expected-panic");
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
        assert_eq!(CONST_FALSE_VALUE, 13);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike MIR instrumentation"]
    fn records_real_runtime_probes() {
        assert_eq!(crate::__supercov_spike_runtime::probe_mask(), 0);
        assert_eq!(authored(true), 1);
        assert_eq!(fallible(2), Ok(3));
        let log = std::cell::RefCell::new(Vec::new());
        assert_eq!(drop_order(&log), 23);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| panic_path(&log)));
        assert!(panic.is_err());
        assert_eq!(crate::__supercov_spike_runtime::probe_mask(), 0b1111);
    }
}
