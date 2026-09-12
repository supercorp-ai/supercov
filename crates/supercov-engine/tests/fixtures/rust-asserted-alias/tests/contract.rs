use asserted_alias_fixture::*;

#[test]
fn copies_and_self_shadowing_preserve_the_original_value() {
    let result = chain(21);
    let first = result;
    let first = first;
    let last = first;
    assert_eq!(last, 42);
}

#[test]
fn boolean_copy_is_exact_too() {
    let result = boolean_chain(1);
    let alias = result;
    assert_eq!(true, alias);
}

#[test]
fn rebinding_the_source_does_not_retarget_a_copy() {
    let result = preserved_source(21);
    let saved = result;
    let result = source_replacement(2);
    let alias = saved;
    assert_eq!(alias, 42);
    std::hint::black_box(result);
}

#[test]
fn rebinding_the_alias_does_not_credit_the_discarded_producer() {
    let result = old_alias(21);
    let alias = result;
    let alias = alias_replacement(2);
    assert_eq!(alias, 4);
}

#[test]
fn a_constant_shadow_does_not_assert_the_producer() {
    let result = constant_shadow(21);
    let alias = result;
    let alias = 42;
    assert_eq!(alias, 42);
}

#[test]
fn an_overwritten_mutable_copy_does_not_assert_the_producer() {
    let result = mutable_alias(21);
    let mut alias = result;
    std::hint::black_box(alias);
    alias = 42;
    assert_eq!(alias, 42);
}

#[test]
fn transforming_a_copy_is_not_exact_observation() {
    let result = transformed_alias(21);
    let alias = result % 2;
    assert_eq!(alias, 0);
}

#[test]
fn nested_block_copy_remains_outside_the_supported_grammar() {
    let result = nested_alias(21);
    let alias = { let nested = result; nested };
    assert_eq!(alias, 42);
}
