use std::{cell::RefCell, panic};

use supercov_rustc_spike_fixture::{authored, drop_order, fallible, panic_path};

fn main() {
    let log = RefCell::new(Vec::new());
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| panic_path(&log)));
    panic::set_hook(previous_hook);

    println!("authored={:?}", [authored(false), authored(true)]);
    println!("fallible={:?}", [fallible(0), fallible(4)]);
    println!("drop-value={}", drop_order(&log));
    println!("panic={}", panic.is_err());
    println!("drop-order={:?}", log.into_inner());
}
