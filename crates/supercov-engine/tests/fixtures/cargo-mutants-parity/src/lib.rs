//! Every construct cargo-mutants mutates, and the ones it leaves alone.
#![allow(dead_code, unused)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub mod operators;
pub mod values;

pub fn unit_body(log: &mut Vec<u8>) {
    log.push(1);
}

pub fn empty() {}

pub fn returns_bool(a: u32, b: u32) -> bool {
    a < b && b != 0 || !(a == b)
}

pub fn always_true() -> bool {
    true
}

pub fn signed(x: i64) -> i64 {
    -x + 2 * x
}

pub fn unsigned(x: usize) -> usize {
    x % 3 / 2
}

pub fn float(x: f64) -> f64 {
    x * 0.5
}

pub fn string(name: &str) -> String {
    format!("hello {}", name.len() + 1)
}

pub fn text() -> &'static str {
    "text"
}

pub fn option(v: u8) -> Option<u8> {
    if v > 3 { Some(v) } else { None }
}

pub fn result(v: u8) -> Result<u8, String> {
    if v >= 3 { Ok(v) } else { Err("small".into()) }
}

pub fn fmt_result() -> std::fmt::Result {
    Ok(())
}

pub fn vector(n: usize) -> Vec<String> {
    vec![String::new(); n]
}

pub fn map() -> HashMap<String, u32> {
    HashMap::new()
}

pub fn btree() -> BTreeMap<u8, bool> {
    BTreeMap::new()
}

pub fn set() -> BTreeSet<i32> {
    BTreeSet::new()
}

pub fn deque() -> VecDeque<u16> {
    VecDeque::new()
}

pub fn hashset<'a>() -> HashSet<&'a str> {
    HashSet::new()
}

pub fn boxed() -> Box<u32> {
    Box::new(3)
}

pub fn shared() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(String::new()))
}

pub fn counted() -> Rc<RefCell<Vec<u8>>> {
    Rc::new(RefCell::new(Vec::new()))
}

pub fn cow(input: &str) -> Cow<'_, str> {
    Cow::Borrowed(input)
}

pub fn tuple() -> (bool, u8) {
    (true, 1)
}

pub fn array() -> [u8; 4] {
    [1; 4]
}

pub fn slice(items: &[u8]) -> &[u8] {
    &items[1..]
}

pub fn mut_slice(items: &mut [u8]) -> &mut [u8] {
    items
}

pub fn reference(value: &u32) -> &u32 {
    value
}

pub fn iterator(items: &[u8]) -> impl Iterator<Item = u8> + '_ {
    items.iter().copied()
}

pub fn displayable() -> impl std::fmt::Display {
    5
}

pub fn never() -> ! {
    panic!("never")
}

pub fn custom() -> Custom {
    Custom { a: 1, b: 2 }
}

pub fn generic_wrapper() -> Wrapper<u8> {
    Wrapper(1)
}

pub fn nonzero() -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(1).unwrap()
}

pub fn nonzero_signed() -> std::num::NonZeroI8 {
    std::num::NonZeroI8::new(1).unwrap()
}

pub unsafe fn unsafe_fn(x: u8) -> u8 {
    x + 1
}

pub const fn const_fn(x: u8) -> u8 {
    x + 1
}

pub const LIMIT: usize = 4 * 1024;

pub async fn asynchronous(x: u8) -> u8 {
    x - 1
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Custom {
    pub a: u8,
    pub b: u8,
}

pub struct Wrapper<T>(pub T);

impl Custom {
    pub fn new() -> Self {
        Custom { a: 1 + 1, b: 0 }
    }

    pub fn sum(&self) -> u16 {
        u16::from(self.a) + u16::from(self.b)
    }

    pub fn with_defaults(a: u8) -> Custom {
        Custom {
            a,
            b: a * 2,
            ..Default::default()
        }
    }

    pub fn update(&mut self, by: u8) {
        self.a += by;
        self.b -= by;
        self.a *= 2;
        self.b /= 2;
        self.a %= 7;
        self.a <<= 1;
        self.a >>= 1;
        self.a &= 3;
        self.a |= 4;
        self.a ^= 5;
    }
}

impl Default for Wrapper<u8> {
    fn default() -> Self {
        Wrapper(0 + 1)
    }
}

impl std::fmt::Display for Custom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.a + self.b)
    }
}

pub trait Shape {
    fn area(&self) -> f32;

    fn double(&self) -> f32 {
        self.area() * 2.0
    }
}

pub fn classify(n: i32) -> &'static str {
    match n {
        0 => "zero",
        1 | 2 => "small",
        x if x < 0 => "negative",
        x if x > 100 && x % 2 == 0 => "big even",
        _ => "other",
    }
}

pub fn exhaustive(flag: Option<bool>) -> u8 {
    match flag {
        Some(true) => 1,
        Some(false) => 2,
        None => 3,
    }
}

pub fn closures(items: &[i32]) -> i32 {
    items.iter().map(|x| x * 2).filter(|x| *x > 1).sum::<i32>() - 1
}

pub fn with_capacity(n: usize) -> Vec<u8> {
    Vec::with_capacity(n + 1)
}

pub fn nested(a: u32) -> u32 {
    fn inner(b: u32) -> u32 {
        b + 1
    }
    inner(a) * 2
}

pub fn bits(a: u8, b: u8) -> u8 {
    (a & b) | (a ^ b) << 1 >> 2
}

pub fn let_chain(a: Option<u8>, b: bool) -> bool {
    if let Some(x) = a && b {
        return x > 1;
    }
    false
}

#[mutants::skip]
pub fn skipped(x: u8) -> u8 {
    x + 1
}

#[cfg_attr(test, mutants::skip)]
pub fn skipped_in_test(x: u8) -> u8 {
    x + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(unsigned(3) + 0, 0);
    }
}
