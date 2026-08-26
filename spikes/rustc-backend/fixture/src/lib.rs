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

macro_rules! generated_match_function {
    () => {
        pub fn generated_match(value: bool) -> usize {
            match value {
                true => 23,
                false => 29,
            }
        }
    };
}

generated_function!();
generated_match_function!();
probe_macros::generated_function!();
probe_macros::generated_match_function!();
probe_macros::generated_guarded_match_function!();
probe_macros::generated_nested_match_function!();
probe_macros::generated_nested_scrutinee_match_function!();
probe_macros::generated_nested_guard_match_function!();
probe_macros::generated_let_else_function!();
probe_macros::generated_two_let_else_function!();
probe_macros::generated_try_function!();
probe_macros::generated_two_try_function!();
probe_macros::generated_nested_try_function!();
probe_macros::generated_assertion_function!();

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

pub const DIRECT_CONST_TRUE: usize = if true { 17 } else { 19 };
pub const DIRECT_CONST_FALSE: usize = if false { 23 } else { 29 };

pub static STATIC_CONST_TRUE: usize = if true { 31 } else { 37 };
pub static STATIC_CONST_FALSE: usize = if false { 41 } else { 43 };

pub const fn const_generic_decision<const ENABLED: bool>() -> usize {
    if ENABLED { 47 } else { 53 }
}

pub const CONST_GENERIC_TRUE: usize = const_generic_decision::<true>();
pub const CONST_GENERIC_FALSE: usize = const_generic_decision::<false>();

pub const fn const_mixed(first: bool, second: bool, third: bool) -> usize {
    if (first || second) && third { 83 } else { 89 }
}

pub const CONST_MIXED_FALSE_FALSE: usize = const_mixed(false, false, true);
pub const CONST_MIXED_SECOND_TRUE: usize = const_mixed(false, true, true);
pub const CONST_MIXED_FIRST_TRUE_FALSE: usize = const_mixed(true, false, false);
pub const CONST_MIXED_FIRST_TRUE_TRUE: usize = const_mixed(true, false, true);

pub const fn const_nested(first: bool, second: bool) -> usize {
    if first {
        if second { 97 } else { 101 }
    } else {
        103
    }
}

pub const CONST_NESTED_OUTER_FALSE: usize = const_nested(false, true);
pub const CONST_NESTED_INNER_FALSE: usize = const_nested(true, false);
pub const CONST_NESTED_INNER_TRUE: usize = const_nested(true, true);

pub const fn const_match(value: u8) -> usize {
    match value {
        0 => 107,
        1 => 109,
        _ => 113,
    }
}

pub const CONST_MATCH_FIRST: usize = const_match(0);
pub const CONST_MATCH_SECOND: usize = const_match(1);
pub const CONST_MATCH_FALLBACK: usize = const_match(2);

pub const fn const_let_else(value: Option<usize>) -> usize {
    let Some(value) = value else {
        return 127;
    };
    value
}

pub const CONST_LET_ELSE_MATCHED: usize = const_let_else(Some(131));
pub const CONST_LET_ELSE_FALLBACK: usize = const_let_else(None);

pub const fn const_while(mut remaining: usize, enabled: bool) -> usize {
    let mut count = 0;
    while remaining > 0 && enabled {
        count += 1;
        remaining -= 1;
    }
    count
}

pub const CONST_WHILE_ZERO: usize = const_while(0, true);
pub const CONST_WHILE_ENTERED: usize = const_while(2, true);
pub const CONST_WHILE_DISABLED: usize = const_while(2, false);

pub struct ConstGenericValue<const ENABLED: bool>;

impl<const ENABLED: bool> ConstGenericValue<ENABLED> {
    pub const VALUE: usize = if ENABLED { 59 } else { 61 };
}

pub const ASSOCIATED_CONST_TRUE: usize = ConstGenericValue::<true>::VALUE;
pub const ASSOCIATED_CONST_FALSE: usize = ConstGenericValue::<false>::VALUE;

pub const ARRAY_DECISION_LEN: usize = [0_u8; if true { 2 } else { 3 }].len();

pub fn inline_const_values() -> [usize; 2] {
    [
        const { if true { 67 } else { 71 } },
        const { if false { 73 } else { 79 } },
    ]
}

pub fn let_else_value(value: Option<usize>) -> usize {
    let Some(value) = value else {
        return 0;
    };
    value
}

pub fn nested_let_else(value: Option<Result<usize, usize>>) -> usize {
    let Some(Ok(value)) = value else {
        return 0;
    };
    value
}

pub fn two_let_else(first: Option<usize>, second: Option<usize>) -> usize {
    let Some(first) = first else {
        return 0;
    };
    let Some(second) = second else {
        return first;
    };
    first + second
}

pub fn try_result(value: Result<usize, &'static str>) -> Result<usize, &'static str> {
    Ok(value? + 1)
}

pub fn try_option(value: Option<usize>) -> Option<usize> {
    Some(value? + 1)
}

pub fn two_try_results(
    first: Result<usize, &'static str>,
    second: Result<usize, &'static str>,
) -> Result<usize, &'static str> {
    Ok(first? + second?)
}

pub fn nested_try_result(
    value: Result<Result<usize, &'static str>, &'static str>,
) -> Result<usize, &'static str> {
    Ok(value?? + 1)
}

pub fn panic_before_try() -> Result<usize, &'static str> {
    fn operand() -> Result<usize, &'static str> {
        panic!("try operand")
    }
    Ok(operand()? + 1)
}

pub fn assert_compound(left: bool, right: bool) {
    assert!(left && right, "compound assertion failed: {left}/{right}");
}

pub fn assert_equal(left: usize, right: usize) {
    assert_eq!(left, right, "values differ");
}

pub fn assert_not_equal(left: usize, right: usize) {
    assert_ne!(left, right, "values unexpectedly equal");
}

pub fn debug_assert_compound(left: bool, right: bool) {
    debug_assert!(left && right, "debug compound assertion failed");
}

pub fn debug_assert_equal(left: usize, right: usize) {
    debug_assert_eq!(left, right, "debug values differ");
}

pub fn debug_assert_not_equal(left: usize, right: usize) {
    debug_assert_ne!(left, right, "debug values unexpectedly equal");
}

pub fn assert_panicking_condition() {
    fn condition() -> bool {
        panic!("assertion condition")
    }
    assert!(condition());
}

pub fn assert_panicking_message_argument() {
    fn message() -> &'static str {
        panic!("assertion message argument")
    }

    assert!(std::hint::black_box(false), "{}", message());
}

pub fn assert_equal_evaluation_order(log: &std::cell::RefCell<Vec<&'static str>>) {
    fn operand(
        log: &std::cell::RefCell<Vec<&'static str>>,
        label: &'static str,
        value: usize,
    ) -> usize {
        log.borrow_mut().push(label);
        value
    }

    assert_eq!(operand(log, "left", 7), operand(log, "right", 7));
}

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

pub fn statement_paths(value: bool, log: &mut Vec<&'static str>) {
    if value {
        log.push("true-path");
    } else {
        log.push("false-path");
    }
    log.push("after-path");
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

pub fn match_value(value: Option<usize>, enabled: bool) -> usize {
    match value {
        Some(value) if value > 0 && enabled => value,
        Some(_) => 2,
        None => 0,
    }
}

pub fn match_identical(value: u8) -> usize {
    match value {
        0 => 7,
        1 => 7,
        _ => 9,
    }
}

pub fn match_empty(value: bool) {
    match value {
        true => {}
        false => {}
    }
}

pub fn match_irrefutable(value: usize) -> usize {
    match value {
        value => value + 1,
    }
}

#[allow(unreachable_patterns)]
pub fn match_unreachable(value: bool) -> usize {
    match value {
        true => 1,
        false => 2,
        _ => 3,
    }
}

pub fn nested_match(value: Option<Result<usize, usize>>) -> usize {
    match value {
        Some(result) => match result {
            Ok(value) => value,
            Err(value) => value + 10,
        },
        None => 0,
    }
}

#[inline(never)]
fn panic_guard() -> bool {
    panic!("match-guard-panic")
}

pub fn interrupted_match(value: Option<usize>) -> usize {
    match value {
        Some(value) if panic_guard() => value,
        _ => 0,
    }
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
        let mut paths = Vec::new();
        statement_paths(true, &mut paths);
        assert_eq!(paths, ["true-path", "after-path"]);
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
    #[test]
    #[ignore = "requires compiler-spike assertion context instrumentation"]
    fn assertion_restore_context() {
        assert_eq!(authored(true), 1);
        let result = fallible(2);
        assert_eq!(result, Ok(3));
    }

    #[cfg(supercov_spike_instrumented)]
    #[test]
    #[ignore = "requires compiler-spike nested assertion context instrumentation"]
    fn nested_assertion_context() {
        assert!({
            assert_eq!(authored(true), 1);
            fallible(2) == Ok(3)
        });
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
