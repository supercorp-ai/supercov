use asserted_identity_fixture::real as compute;

#[test]
fn renamed_direct_operand() {
    assert_eq!(compute(21), 42);
}

#[test]
fn renamed_precomputed_copy() {
    let result = compute(21);
    let alias = result;
    assert_eq!(alias, 42);
}
