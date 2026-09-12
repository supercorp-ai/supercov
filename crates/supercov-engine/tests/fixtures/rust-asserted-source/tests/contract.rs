use asserted_source_fixture::*;

#[test]
fn reversed_qualified_equality() {
    std::assert_eq!(-4, asserted_source_fixture::reversed(-2));
}

#[test]
fn immutable_boolean_local() {
    let result = local_boolean(1);
    assert_eq!(result, true);
}

#[test]
fn exact_zero_is_not_a_mutation_guarantee() {
    assert_eq!(zero_exact(0), 0);
}

#[test]
fn masked_value() {
    assert_eq!(masked(21) & 1, 0);
}

#[test]
fn discarded_value() {
    assert_eq!({ discarded(21); 42 }, 42);
}

#[test]
fn overwritten_value() {
    let mut result = mutable_overwritten(21);
    std::hint::black_box(result);
    result = 42;
    assert_eq!(result, 42);
}

#[test]
fn copied_alias_is_outside_this_recognizer() {
    let result = aliased(21);
    let alias = result;
    assert_eq!(alias, 42);
}

#[test]
fn hoisted_local_function_shadows_the_library() {
    assert_eq!(shadowed(21), 42);
    fn shadowed(value: i32) -> i32 {
        asserted_source_fixture::shadowed(value);
        42
    }
}

#[test]
fn co_varying_expected_is_not_a_literal_oracle() {
    assert_eq!(co_varying(21), co_varying(21));
}

#[test]
fn floating_equality_is_outside_this_recognizer() {
    assert_eq!(float_result(21.0), 42.0);
}

#[test]
fn overloaded_equality_checks_no_returned_bits() {
    assert_eq!(custom_result(21), 0);
}
