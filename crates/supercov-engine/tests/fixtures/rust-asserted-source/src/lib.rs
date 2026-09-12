pub fn reversed(value: i32) -> i32 { value * 2 }
pub fn local_boolean(value: i32) -> bool { value > 0 }
pub fn zero_exact(value: i32) -> i32 { value * 2 }
pub fn masked(value: i32) -> i32 { value * 2 }
pub fn discarded(value: i32) -> i32 { value * 2 }
pub fn mutable_overwritten(value: i32) -> i32 { value * 2 }
pub fn aliased(value: i32) -> i32 { value * 2 }
pub fn shadowed(value: i32) -> i32 { value * 2 }
pub fn co_varying(value: i32) -> i32 { value * 2 }
pub fn float_result(value: f64) -> f64 { value * 2.0 }
pub fn custom_result(value: i32) -> Always { Always { value: value * 2 } }

pub struct Always { pub value: i32 }

impl std::fmt::Debug for Always {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Always")
    }
}

impl PartialEq<i32> for Always {
    fn eq(&self, _: &i32) -> bool { true }
}
