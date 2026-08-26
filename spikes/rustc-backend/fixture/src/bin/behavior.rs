use std::{cell::RefCell, panic};

use supercov_rustc_spike_fixture::{
    CONST_FALSE_VALUE, CONST_VALUE, authored, context_normal_scope, context_panic_scope,
    drop_order, fallible, panic_path,
};

fn main() {
    let log = RefCell::new(Vec::new());
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| panic_path(&log)));
    assert_eq!(context_normal_scope(), 1);
    let context_log = RefCell::new(Vec::new());
    let context_panic =
        panic::catch_unwind(panic::AssertUnwindSafe(|| context_panic_scope(&context_log)));
    panic::set_hook(previous_hook);
    assert!(context_panic.is_err());
    assert_eq!(authored(true), 1);

    println!("authored={:?}", [authored(false), authored(true)]);
    println!("fallible={:?}", [fallible(0), fallible(4)]);
    println!("drop-value={}", drop_order(&log));
    println!("panic={}", panic.is_err());
    println!("drop-order={:?}", log.into_inner());
    println!("const-values={CONST_VALUE:?},{CONST_FALSE_VALUE:?}");
}
