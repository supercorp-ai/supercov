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

pub mod repeated_expansions {
    generated_function!();
    probe_macros::generated_function!();
}

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

pub fn compound(left: bool, right: bool) -> usize {
    if left && right { 29 } else { 31 }
}

pub fn disjoined(left: bool, right: bool) -> usize {
    if left || right { 47 } else { 49 }
}

pub fn mixed(first: bool, second: bool, third: bool) -> usize {
    if (first || second) && third { 53 } else { 59 }
}

pub fn nested(first: bool, second: bool, third: bool) -> usize {
    if first {
        if second && third { 71 } else { 73 }
    } else {
        79
    }
}

pub fn nested_expression(first: bool, second: bool, third: bool, fourth: bool) -> usize {
    if first && (if second { third } else { fourth }) {
        83
    } else {
        89
    }
}

pub fn while_compound(mut remaining: usize, enabled: bool) -> usize {
    let mut entered = 0;
    while remaining > 0 && enabled {
        entered += 1;
        remaining -= 1;
    }
    entered
}

pub fn while_let_chain(mut values: Vec<Option<usize>>, enabled: bool) -> usize {
    let mut total = 0;
    while let Some(Some(value)) = values.pop()
        && value > 0
        && enabled
    {
        total += value;
    }
    total
}

pub fn for_values(values: Vec<usize>) -> usize {
    let mut total = 0;
    for value in values {
        total += value;
    }
    total
}

pub fn for_break(values: Vec<usize>) -> usize {
    for value in values {
        return value;
    }
    0
}

pub fn two_for_values(first: Vec<usize>, second: Vec<usize>) -> usize {
    let mut total = 0;
    for value in first {
        total += value;
    }
    for value in second {
        total += value;
    }
    total
}

pub fn nested_for_values(rows: Vec<Vec<usize>>) -> usize {
    let mut total = 0;
    for row in rows {
        for value in row {
            total += value;
        }
    }
    total
}

struct PanicOnNext;

impl Iterator for PanicOnNext {
    type Item = usize;

    #[inline(never)]
    fn next(&mut self) -> Option<Self::Item> {
        panic!("iterator-next-panic")
    }
}

pub fn interrupted_for() {
    for _value in PanicOnNext {}
}

#[inline(never)]
fn panic_condition() -> bool {
    panic!("decision-condition-panic")
}

pub fn interrupted_decision(first: bool) -> usize {
    if first && panic_condition() { 61 } else { 67 }
}

pub fn pattern(value: Option<bool>) -> usize {
    if let Some(value) = value {
        usize::from(value)
    } else {
        37
    }
}

pub fn chained(value: Option<bool>, enabled: bool) -> usize {
    if let Some(value) = value
        && value
        && enabled
    {
        41
    } else {
        43
    }
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

pub fn context_normal_scope() -> usize {
    authored(true)
}

pub fn context_panic_scope(log: &std::cell::RefCell<Vec<&'static str>>) {
    panic_path(log);
}

#[cfg(test)]
mod tests {
    use super::*;

    static CONTEXT_BARRIER: std::sync::Barrier = std::sync::Barrier::new(2);

    #[test]
    fn exercises_every_surface() {
        assert_eq!(authored(true), 1);
        assert_eq!(generated_by_rules(false), 5);
        assert_eq!(generated_by_proc(true), 17);
        assert_eq!(generated_by_build_script(false), 9);
        assert_eq!(CONST_VALUE, 11);
        assert_eq!(CONST_FALSE_VALUE, 13);
        assert_eq!(compound(true, true), 29);
        assert_eq!(compound(true, false), 31);
        assert_eq!(disjoined(true, false), 47);
        assert_eq!(disjoined(false, false), 49);
        assert_eq!(mixed(false, true, true), 53);
        assert_eq!(mixed(false, false, true), 59);
        assert_eq!(nested(false, true, true), 79);
        assert_eq!(nested(true, true, true), 71);
        assert_eq!(nested(true, true, false), 73);
        assert_eq!(nested_expression(false, true, true, true), 89);
        assert_eq!(nested_expression(true, true, true, false), 83);
        assert_eq!(nested_expression(true, true, false, true), 89);
        assert_eq!(nested_expression(true, false, false, true), 83);
        assert_eq!(while_compound(0, true), 0);
        assert_eq!(while_compound(2, true), 2);
        assert_eq!(while_compound(2, false), 0);
        assert_eq!(while_let_chain(Vec::new(), true), 0);
        assert_eq!(while_let_chain(vec![Some(2), Some(3)], true), 5);
        assert_eq!(while_let_chain(vec![Some(2)], false), 0);
        assert_eq!(while_let_chain(vec![Some(0)], true), 0);
        assert_eq!(pattern(Some(true)), 1);
        assert_eq!(pattern(None), 37);
        assert_eq!(chained(Some(true), true), 41);
        assert_eq!(chained(Some(false), true), 43);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike MIR instrumentation"]
    fn records_real_runtime_probes() {
        assert_eq!(authored(true), 1);
        assert_eq!(fallible(2), Ok(3));
        let log = std::cell::RefCell::new(Vec::new());
        assert_eq!(drop_order(&log), 23);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| panic_path(&log)));
        assert!(panic.is_err());
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn context_one() {
        CONTEXT_BARRIER.wait();
        assert_eq!(authored(true), 1);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn context_two() {
        CONTEXT_BARRIER.wait();
        assert_eq!(fallible(2), Ok(3));
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn decision_context_true() {
        assert_eq!(compound(true, true), 29);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn decision_context_short_circuit() {
        assert_eq!(compound(false, true), 31);
    }

    #[cfg(supercov_spike_instrumented)]
    #[probe_macros::generated_test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn attribute_context() {
        assert_eq!(authored(false), 2);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    #[should_panic(expected = "expected-panic")]
    fn panic_context() {
        let log = std::cell::RefCell::new(Vec::new());
        panic_path(&log);
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike context instrumentation"]
    fn child_context() {
        assert_eq!(std::thread::spawn(|| authored(true)).join().unwrap(), 1);
    }
}
