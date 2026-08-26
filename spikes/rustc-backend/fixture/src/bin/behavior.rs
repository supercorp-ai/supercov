use std::{cell::RefCell, panic};

use supercov_rustc_spike_fixture::{
    ARRAY_DECISION_LEN, ASSOCIATED_CONST_FALSE, ASSOCIATED_CONST_TRUE, CONST_ASSERTION_FIRST,
    CONST_ASSERTION_SECOND, CONST_DEBUG_ASSERTION_FIRST, CONST_DEBUG_ASSERTION_SECOND,
    CONST_FALSE_VALUE, CONST_GENERIC_FALSE, CONST_GENERIC_TRUE, CONST_LET_ELSE_FALLBACK,
    CONST_LET_ELSE_MATCHED, CONST_MATCH_FALLBACK, CONST_MATCH_FIRST, CONST_MATCH_SECOND,
    CONST_MIXED_FALSE_FALSE, CONST_MIXED_FIRST_TRUE_FALSE, CONST_MIXED_FIRST_TRUE_TRUE,
    CONST_MIXED_SECOND_TRUE, CONST_NESTED_INNER_FALSE, CONST_NESTED_INNER_TRUE,
    CONST_NESTED_OUTER_FALSE, CONST_VALUE, CONST_WHILE_DISABLED, CONST_WHILE_ENTERED,
    CONST_WHILE_ZERO, DIRECT_CONST_ASSERTION, DIRECT_CONST_DEBUG_ASSERTION, DIRECT_CONST_FALSE,
    DIRECT_CONST_TRUE, STATIC_CONST_FALSE, STATIC_CONST_TRUE, assert_compound, assert_equal,
    assert_equal_evaluation_order, assert_not_equal, assert_panicking_condition,
    assert_panicking_message_argument, authored, chained, compound, context_normal_scope,
    context_panic_scope, debug_assert_compound, debug_assert_equal, debug_assert_not_equal,
    disjoined, drop_order, fallible, for_break, for_values, generated_assertion_by_proc,
    generated_by_build_script, generated_by_proc, generated_by_rules,
    generated_guarded_match_by_proc, generated_let_else_by_proc, generated_match,
    generated_match_by_proc, generated_nested_guard_match_by_proc, generated_nested_match_by_proc,
    generated_nested_scrutinee_match_by_proc, generated_nested_try_by_proc, generated_try_by_proc,
    generated_two_let_else_by_proc, generated_two_try_by_proc, inline_const_values,
    interrupted_decision, interrupted_for, interrupted_match, let_else_value, match_empty,
    match_identical, match_irrefutable, match_unreachable, match_value, mixed, nested,
    nested_expression, nested_for_values, nested_let_else, nested_match, nested_try_result,
    panic_before_try, panic_path, pattern, repeated_expansions, try_option, try_result,
    two_for_values, two_let_else, two_try_results, while_compound, while_let_chain,
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
    let interrupted_for = panic::catch_unwind(interrupted_for);
    let interrupted_match = panic::catch_unwind(|| interrupted_match(Some(3)));
    let assertion_panics = [
        panic::catch_unwind(|| assert_compound(false, true)).is_err(),
        panic::catch_unwind(|| assert_compound(true, false)).is_err(),
        panic::catch_unwind(|| assert_compound(true, true)).is_err(),
        panic::catch_unwind(|| assert_equal(1, 1)).is_err(),
        panic::catch_unwind(|| assert_equal(1, 2)).is_err(),
        panic::catch_unwind(|| assert_not_equal(1, 2)).is_err(),
        panic::catch_unwind(|| assert_not_equal(1, 1)).is_err(),
        panic::catch_unwind(|| debug_assert_compound(false, true)).is_err(),
        panic::catch_unwind(|| debug_assert_compound(true, false)).is_err(),
        panic::catch_unwind(|| debug_assert_compound(true, true)).is_err(),
        panic::catch_unwind(|| debug_assert_equal(1, 1)).is_err(),
        panic::catch_unwind(|| debug_assert_equal(1, 2)).is_err(),
        panic::catch_unwind(|| debug_assert_not_equal(1, 2)).is_err(),
        panic::catch_unwind(|| debug_assert_not_equal(1, 1)).is_err(),
        panic::catch_unwind(|| generated_assertion_by_proc(false, true)).is_err(),
        panic::catch_unwind(|| generated_assertion_by_proc(true, false)).is_err(),
        panic::catch_unwind(|| generated_assertion_by_proc(true, true)).is_err(),
    ];
    let assertion_condition_panic = panic::catch_unwind(assert_panicking_condition);
    let assertion_message_panic = panic::catch_unwind(assert_panicking_message_argument);
    let assertion_order = RefCell::new(Vec::new());
    assert_equal_evaluation_order(&assertion_order);
    panic::set_hook(previous_hook);
    assert!(context_panic.is_err());
    assert_eq!(authored(true), 1);

    println!("authored={:?}", [authored(false), authored(true)]);
    println!("fallible={:?}", [fallible(0), fallible(4)]);
    println!("drop-value={}", drop_order(&log));
    println!("panic={}", panic.is_err());
    println!("decision-panic={}", interrupted.is_err());
    println!("for-panic={}", interrupted_for.is_err());
    println!("match-panic={}", interrupted_match.is_err());
    println!("assertion-panics={assertion_panics:?}");
    println!(
        "assertion-edge-panics={:?}",
        [
            assertion_condition_panic.is_err(),
            assertion_message_panic.is_err()
        ]
    );
    println!("assertion-order={:?}", assertion_order.into_inner());
    println!("drop-order={:?}", log.into_inner());
    println!("const-values={CONST_VALUE:?},{CONST_FALSE_VALUE:?}");
    println!(
        "ctfe-surfaces={:?}",
        [
            DIRECT_CONST_TRUE,
            DIRECT_CONST_FALSE,
            STATIC_CONST_TRUE,
            STATIC_CONST_FALSE,
            CONST_GENERIC_TRUE,
            CONST_GENERIC_FALSE,
            ASSOCIATED_CONST_TRUE,
            ASSOCIATED_CONST_FALSE,
            ARRAY_DECISION_LEN,
            inline_const_values()[0],
            inline_const_values()[1],
            CONST_MIXED_FALSE_FALSE,
            CONST_MIXED_SECOND_TRUE,
            CONST_MIXED_FIRST_TRUE_FALSE,
            CONST_MIXED_FIRST_TRUE_TRUE,
            CONST_NESTED_OUTER_FALSE,
            CONST_NESTED_INNER_FALSE,
            CONST_NESTED_INNER_TRUE,
            CONST_MATCH_FIRST,
            CONST_MATCH_SECOND,
            CONST_MATCH_FALLBACK,
            CONST_LET_ELSE_MATCHED,
            CONST_LET_ELSE_FALLBACK,
            CONST_WHILE_ZERO,
            CONST_WHILE_ENTERED,
            CONST_WHILE_DISABLED,
            CONST_ASSERTION_FIRST,
            CONST_ASSERTION_SECOND,
            DIRECT_CONST_ASSERTION,
            CONST_DEBUG_ASSERTION_FIRST,
            CONST_DEBUG_ASSERTION_SECOND,
            DIRECT_CONST_DEBUG_ASSERTION,
        ]
    );
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
    println!(
        "while={:?}",
        [
            while_compound(0, true),
            while_compound(2, true),
            while_compound(2, false),
        ]
    );
    println!(
        "while-let={:?}",
        [
            while_let_chain(Vec::new(), true),
            while_let_chain(vec![Some(2), Some(3)], true),
            while_let_chain(vec![Some(2)], false),
            while_let_chain(vec![Some(0)], true),
        ]
    );
    println!("for={:?}", [for_values(Vec::new()), for_values(vec![2, 3])]);
    println!(
        "for-break={:?}",
        [for_break(Vec::new()), for_break(vec![7, 9])]
    );
    println!(
        "for-two={:?}",
        [
            two_for_values(Vec::new(), vec![2]),
            two_for_values(vec![3], Vec::new()),
        ]
    );
    println!(
        "for-nested={:?}",
        [
            nested_for_values(Vec::new()),
            nested_for_values(vec![Vec::new(), vec![2, 3]]),
        ]
    );
    println!(
        "match={:?}",
        [
            match_value(Some(3), true),
            match_value(Some(0), true),
            match_value(Some(3), false),
            match_value(None, true),
        ]
    );
    println!(
        "match-identical={:?}",
        [match_identical(0), match_identical(1), match_identical(2)]
    );
    match_empty(true);
    match_empty(false);
    println!("match-empty=true");
    println!("match-irrefutable={}", match_irrefutable(4));
    println!(
        "match-unreachable={:?}",
        [match_unreachable(true), match_unreachable(false)]
    );
    println!(
        "match-generated={:?}",
        [generated_match(true), generated_match(false)]
    );
    println!(
        "match-generated-proc={:?}",
        [
            generated_match_by_proc(true),
            generated_match_by_proc(false)
        ]
    );
    println!(
        "match-generated-guarded-proc={:?}",
        [
            generated_guarded_match_by_proc(Some(3), true),
            generated_guarded_match_by_proc(Some(0), true),
            generated_guarded_match_by_proc(Some(3), false),
            generated_guarded_match_by_proc(None, true),
        ]
    );
    println!(
        "match-generated-nested-proc={:?}",
        [
            generated_nested_match_by_proc(Some(Ok(3))),
            generated_nested_match_by_proc(Some(Err(4))),
            generated_nested_match_by_proc(None),
        ]
    );
    println!(
        "match-generated-nested-scrutinee-proc={:?}",
        [
            generated_nested_scrutinee_match_by_proc(Some(true)),
            generated_nested_scrutinee_match_by_proc(Some(false)),
            generated_nested_scrutinee_match_by_proc(None),
        ]
    );
    println!(
        "match-generated-nested-guard-proc={:?}",
        [
            generated_nested_guard_match_by_proc(Some(3), true),
            generated_nested_guard_match_by_proc(Some(0), true),
            generated_nested_guard_match_by_proc(Some(3), false),
            generated_nested_guard_match_by_proc(None, true),
        ]
    );
    println!(
        "let-else={:?}",
        [let_else_value(Some(7)), let_else_value(None)]
    );
    println!(
        "let-else-nested={:?}",
        [
            nested_let_else(Some(Ok(7))),
            nested_let_else(Some(Err(7))),
            nested_let_else(None),
        ]
    );
    println!(
        "let-else-two={:?}",
        [
            two_let_else(Some(2), Some(3)),
            two_let_else(None, Some(3)),
            two_let_else(Some(2), None),
        ]
    );
    println!(
        "let-else-generated-proc={:?}",
        [
            generated_let_else_by_proc(Some(7)),
            generated_let_else_by_proc(None)
        ]
    );
    println!(
        "let-else-generated-two-proc={:?}",
        [
            generated_two_let_else_by_proc(Some(Ok(2)), Some(3)),
            generated_two_let_else_by_proc(None, Some(3)),
            generated_two_let_else_by_proc(Some(Ok(2)), None),
        ]
    );
    println!(
        "try-result={:?}",
        [try_result(Ok(7)), try_result(Err("no"))]
    );
    println!("try-option={:?}", [try_option(Some(7)), try_option(None)]);
    println!(
        "try-two={:?}",
        [
            two_try_results(Ok(2), Ok(3)),
            two_try_results(Err("first"), Ok(3)),
            two_try_results(Ok(2), Err("second")),
        ]
    );
    println!(
        "try-generated-proc={:?}",
        [
            generated_try_by_proc(Ok(8)),
            generated_try_by_proc(Err("no"))
        ]
    );
    println!(
        "try-generated-two-proc={:?}",
        [
            generated_two_try_by_proc(Ok(2), Ok(3)),
            generated_two_try_by_proc(Err("first"), Ok(3)),
            generated_two_try_by_proc(Ok(2), Err("second")),
        ]
    );
    println!(
        "try-nested={:?}",
        [
            nested_try_result(Ok(Ok(7))),
            nested_try_result(Ok(Err("inner"))),
            nested_try_result(Err("outer")),
        ]
    );
    println!(
        "try-generated-nested-proc={:?}",
        [
            generated_nested_try_by_proc(Ok(Ok(7))),
            generated_nested_try_by_proc(Ok(Err("inner"))),
            generated_nested_try_by_proc(Err("outer")),
        ]
    );
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let try_panic = panic::catch_unwind(panic_before_try);
    panic::set_hook(previous_hook);
    println!("try-panic={}", try_panic.is_err());
    println!(
        "match-nested={:?}",
        [
            nested_match(Some(Ok(3))),
            nested_match(Some(Err(4))),
            nested_match(None),
        ]
    );
}
