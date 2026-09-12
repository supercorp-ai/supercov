use asserted_runtime_fixture::*;

#[test]
fn exact_result_in_operand() {
    assert_eq!(direct(21), 42);
    assert_eq!(direct(2), 4);
}

#[test]
fn exact_result_precomputed() {
    let result = precomputed(21);
    assert_eq!(result, 42);
}

#[test]
fn execution_only() {
    std::hint::black_box(smoke(21));
}

#[test]
fn discarded_operand_result() {
    assert_eq!({ discarded(21); 42 }, 42);
}

#[test]
fn masked_operand_result() {
    assert_eq!(masked(21) % 2, 0);
}

#[test]
fn weak_inequality() {
    assert_ne!(weak(21), -999);
}

#[test]
fn exact_result_on_joined_thread() {
    let thread = std::thread::spawn(|| threaded(21));
    assert_eq!(thread.join().unwrap(), 42);
}
