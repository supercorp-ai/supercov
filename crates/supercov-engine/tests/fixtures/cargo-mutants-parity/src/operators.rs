//! Operators in every position.

pub fn compare(a: i32, b: i32) -> u8 {
    let mut n = 0;
    if a < b {
        n += 1;
    }
    if a <= b {
        n += 2;
    }
    if a > b {
        n += 4;
    }
    if a >= b {
        n += 8;
    }
    if a == b || a != -b {
        n += 16;
    }
    n
}

pub fn arithmetic(a: i64, b: i64) -> i64 {
    let sum = a + b;
    let difference = a - b;
    let product = a * b;
    let quotient = if b != 0 { a / b } else { 0 };
    let remainder = if b != 0 { a % b } else { 0 };
    sum + difference + product + quotient + remainder
}

pub fn macros(a: u32) -> String {
    let v = vec![a + 1, a - 1];
    assert!(a < 100);
    format!("{} {}", v[0] * 2, a == 3)
}

pub fn negations(a: bool, b: i32) -> (bool, i32) {
    (!a, -b)
}

pub fn deref(x: &i32) -> i32 {
    *x
}

pub(crate) fn ranges(n: usize) -> usize {
    let mut total = 0;
    for i in 0..n {
        total += i;
    }
    while total > 10 {
        total -= 3;
    }
    total
}

pub fn strings(a: &str, b: &str) -> String {
    a.to_owned() + b
}

pub fn chained(a: u8, b: u8, c: u8) -> bool {
    a + b * c == c - a / b
}
