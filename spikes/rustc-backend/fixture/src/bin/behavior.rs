use std::{cell::RefCell, panic};

use supercov_rustc_spike_fixture::{
    CONST_FALSE_VALUE, CONST_VALUE, authored, chained, compound, context_normal_scope,
    context_panic_scope, disjoined, drop_order, fallible, generated_by_build_script,
    generated_by_proc, generated_by_rules, interrupted_decision, mixed, nested,
    nested_expression, panic_path, pattern, repeated_expansions,
};

fn main() {
    let log = RefCell::new(Vec::new());
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| panic_path(&log)));
    assert_eq!(context_normal_scope(), 1);
    let context_log = RefCell::new(Vec::new());
    let context_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        context_panic_scope(&context_log)
    }));
    let interrupted = panic::catch_unwind(|| interrupted_decision(true));
    panic::set_hook(previous_hook);
    assert!(context_panic.is_err());
    assert_eq!(authored(true), 1);

    println!("authored={:?}", [authored(false), authored(true)]);
    println!("fallible={:?}", [fallible(0), fallible(4)]);
    println!("drop-value={}", drop_order(&log));
    println!("panic={}", panic.is_err());
    println!("decision-panic={}", interrupted.is_err());
    println!("drop-order={:?}", log.into_inner());
    println!("const-values={CONST_VALUE:?},{CONST_FALSE_VALUE:?}");
    println!(
        "expanded={:?}",
        [
            generated_by_rules(false),
            repeated_expansions::generated_by_rules(true),
            generated_by_proc(false),
            repeated_expansions::generated_by_proc(true),
            generated_by_build_script(false),
        ]
    );
    println!(
        "conditions={:?}",
        [
            compound(true, true),
            compound(true, false),
            compound(false, true),
            pattern(Some(true)),
            pattern(None),
            chained(Some(true), true),
            chained(Some(false), true),
            chained(None, true),
        ]
    );
    println!(
        "or-mixed={:?}",
        [
            disjoined(true, false),
            disjoined(false, true),
            disjoined(false, false),
            mixed(true, false, true),
            mixed(true, false, false),
            mixed(false, true, true),
            mixed(false, false, true),
        ]
    );
    println!(
        "nested={:?}",
        [
            nested(false, true, true),
            nested(true, true, true),
            nested(true, true, false),
            nested(true, false, true),
        ]
    );
    println!(
        "nested-expression={:?}",
        [
            nested_expression(false, true, true, true),
            nested_expression(true, true, true, false),
            nested_expression(true, true, false, true),
            nested_expression(true, false, false, true),
            nested_expression(true, false, false, false),
        ]
    );
}
