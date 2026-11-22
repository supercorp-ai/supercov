//! Return types and attributes at the edges of what cargo-mutants reads.
//! Only `cargo mutants --list` reads this file: it need not compile.

use std::collections::HashMap;
use std::num::NonZero;

pub fn nonzero_unsigned() -> NonZero<u8> {
    NonZero::new(1).unwrap()
}

pub fn nonzero_signed() -> NonZero<i32> {
    NonZero::new(1).unwrap()
}

pub fn nonzero_array() -> NonZero<[u8; 1]> {
    todo!()
}

pub fn nonzero_bare() -> NonZero {
    todo!()
}

pub fn option_past_lifetime<'a>() -> Option<'a, u8> {
    todo!()
}

pub fn option_only_lifetime<'a>() -> Option<'a> {
    todo!()
}

pub fn option_const() -> Option<3> {
    todo!()
}

pub fn option_empty() -> Option<> {
    todo!()
}

pub fn http() -> HttpResponse {
    todo!()
}

pub fn boxed_two() -> Box<u8, Global> {
    todo!()
}

pub fn boxed_bare() -> Box {
    todo!()
}

pub fn map_one() -> HashMap<u8> {
    todo!()
}

pub fn map_bare() -> HashMap {
    todo!()
}

pub fn holder_lifetime<'a>() -> Holder<'a, u8> {
    todo!()
}

pub fn holder_two() -> Pair<u8, u16> {
    todo!()
}

pub fn iter_with_lifetime<'a>() -> impl Iterator<Item = u8> + 'a {
    todo!()
}

pub fn iter_with_use<'a>() -> impl use<'a> + Iterator<Item = u8> {
    todo!()
}

pub fn iter_type_argument() -> impl Iterator<u8> {
    todo!()
}

pub fn iter_other_name() -> impl Iterator<Other = u8> {
    todo!()
}

pub fn iter_for() -> impl for<'a> Fn(&'a u8) -> u8 {
    todo!()
}

pub fn iter_parenthesized() -> impl (Iterator<Item = u8>) {
    todo!()
}

pub fn commented() -> u8 {
    // plain
    fn helper() {}
    1
}

pub fn documented() -> u8 {
    /// doc
    fn helper() {}
    1
}

pub fn attributed_operators(a: u8) -> u8 {
    #[cfg(test)]
    a + 1;
    let b = #[mutants::skip] a * 2;
    b - 1
}

pub fn never(x: Never) -> u8 {
    match x {}
}

pub fn spread(s: S) -> S {
    S { ..s }
}

#[test]
fn a_test() -> u8 {
    1 + 1
}

#[cfg(_private)]
pub fn underscore_cfg() -> u8 {
    1 + 1
}

#[allow(clippy::)]
pub fn path_cut_short() -> u8 {
    1 + 1
}

#[allow()]
pub fn empty_cfg() -> u8 {
    1 + 1
}

const _: () = assert!(LIMIT > 1 + 1);

pub const LIMIT: u32 = 3;

pub struct Limited;

impl Limited {
    const LIMIT: u32 = 1 + 0;

    pub fn limit() -> u32 {
        Self::LIMIT * 2
    }
}

pub fn count() -> u32 {
    let mut seen = 0;
    for _ in 0..LIMIT {
        seen += 1;
    }
    seen
}
