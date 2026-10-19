// Every construct the Rust instrumenter rewrites, measured end to end.
//
//   node scripts/rust-syntax-zoo.mjs
//
// A small crate whose tests drive each construct both ways: let chains,
// `while let`, a labelled loop broken with a value, match guards and ranges,
// `?` on Result and Option, `matches!`, `&&` and `||` used as values,
// `vec![x; n]`, a closure returned as
// `impl Fn`, a statement longer than a line, a trait with a declared and a
// default method, an extern declaration. Each file is read back for what its
// tests showed; const contexts and macro expansions, which cannot hold a
// probe, must say so instead.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { assessWithStandIn } from './assess-with-stand-in.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-rust-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

const files = {
  'Cargo.toml': '[package]\nname = "zoo"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n',
  'src/lib.rs': 'pub mod consts;\npub mod flow;\npub mod macros;\npub mod shapes;\npub mod tail;\npub mod values;\n',
  // The long tail: a \`break\` that belongs to an inner \`for\`, \`while\` or
  // closure, a let chain in a \`const fn\` no probe can enter, an attributed
  // statement macro closing a block, and an assertion that closes one.
  'src/tail.rs': `pub fn first_hit(rows: &[Vec<u32>]) -> Option<u32> {
    let mut found = None;
    loop {
        for row in rows {
            for value in row {
                if *value > 5 {
                    found = Some(*value);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let mut n = 0;
        while n < rows.len() {
            n += 1;
            if n == 2 {
                break;
            }
        }
        let stop = || {
            let mut i = 0;
            loop {
                i += 1;
                if i > 1 {
                    break i;
                }
            }
        };
        let _ = stop();
        break found;
    }
}

pub const fn chained(value: Option<u8>) -> u8 {
    if let Some(v) = value && v > 1 { v } else { 0 }
}

pub fn logged(x: u32) -> u32 {
    if x > 0 {
        #[rustfmt::skip]
        println! { "{x}" }
    }
    x
}

pub fn checked(x: u32) {
    assert!(x < 100)
}
`,
  'src/flow.rs': `pub fn pick(value: Option<u32>, limit: u32) -> &'static str {
    if let Some(v) = value
        && v > limit
    {
        "over"
    } else if value.is_some() {
        "under"
    } else {
        "none"
    }
}

pub fn sum_until(values: &[i32], stop: i32) -> i32 {
    let mut total = 0;
    let mut iter = values.iter();
    while let Some(&value) = iter.next() {
        if value == stop {
            break;
        }
        total += value;
    }
    total
}

pub fn find(grid: &[Vec<u32>], target: u32) -> Option<(usize, usize)> {
    'outer: loop {
        for (row, cells) in grid.iter().enumerate() {
            for (column, cell) in cells.iter().enumerate() {
                if *cell == target {
                    break 'outer Some((row, column));
                }
            }
        }
        break None;
    }
}

pub fn classify(n: i32) -> &'static str {
    match n {
        x if x < 0 => "negative",
        0 => "zero",
        1..=9 => "small",
        _ => "large",
    }
}

pub fn add(a: &str, b: &str) -> Result<u32, std::num::ParseIntError> {
    let x: u32 = a.trim().parse()?;
    let y: u32 = b.trim().parse()?;
    Ok(x + y)
}

pub fn doubled(value: Option<u32>) -> Option<u32> {
    let x = value?;
    Some(x * 2)
}

pub fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

pub fn zeros(n: usize) -> Vec<u8> {
    vec![0; n]
}

pub fn adder(step: u32) -> impl Fn(u32) -> u32 {
    move |x| if step > 0 { x + step } else { x }
}

pub fn on_axis(x: i32, y: i32) -> bool {
    x == 0 || y == 0
}

pub fn both(a: bool, b: bool) -> bool {
    let ok = a && b;
    ok
}

pub fn in_band(values: &[i32]) -> Vec<i32> {
    let keep = |v: &i32| *v > 0 && *v < 10;
    values.iter().copied().filter(|v| keep(v)).collect()
}

pub fn gate(a: bool, b: bool, c: bool) -> &'static str {
    if std::convert::identity(a || b) && c { "open" } else { "shut" }
}

pub fn describe(name: &str, count: usize) -> String {
    if count > 1 { format!("{name} has {count} items, which is more than one, and so this sentence uses the plural form of the noun") } else { format!("{name} has one item") }
}
`,
  'src/shapes.rs': `pub trait Area {
    fn area(&self) -> f64;

    fn size(&self) -> &'static str {
        if self.area() > 10.0 { "big" } else { "small" }
    }
}

pub struct Square(pub f64);

impl Area for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}

unsafe extern "C" {
    fn abs(x: i32) -> i32;
}

pub fn magnitude(x: i32) -> i32 {
    unsafe { abs(x) }
}
`,
  'src/consts.rs': `pub const LIMIT: u32 = if cfg!(debug_assertions) { 10 } else { 20 };

pub const fn clamp(x: u32) -> u32 {
    if x > LIMIT { LIMIT } else { x }
}

pub static TABLE: [u8; 4] = [1, 2, 3, 4];

pub fn lookup(index: usize) -> Option<u8> {
    TABLE.get(index).copied()
}

// Code that runs at compile time, or inside the allocator, cannot carry a
// probe: each shape below must still compile and behave.
pub const fn first_even(values: &[u32]) -> Option<u32> {
    let mut i = 0;
    while i < values.len() {
        if values[i] % 2 == 0 && values[i] > 0 {
            return Some(values[i]);
        }
        i += 1;
    }
    None
}

pub const fn sign(x: i32) -> i32 {
    match x {
        n if n > 0 => 1,
        0 => 0,
        _ => -1,
    }
}

pub const fn halvings(mut n: u32) -> u32 {
    let mut count = 0;
    loop {
        if n == 0 {
            break count;
        }
        n /= 2;
        count += 1;
    }
}

pub const SQUARES: [u32; 3] = {
    let mut squares = [0; 3];
    let mut i = 0;
    while i < 3 {
        squares[i] = (i * i) as u32;
        i += 1;
    }
    squares
};

pub fn inline_const() -> u32 {
    const { if LIMIT > 5 { 2 } else { 3 } }
}

pub fn sized() -> [u8; if LIMIT > 5 { 2 } else { 3 }] {
    [0; if LIMIT > 5 { 2 } else { 3 }]
}

pub struct Counting;

unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if layout.size() > 0 && layout.align() <= 16 {
            unsafe { std::alloc::GlobalAlloc::alloc(&std::alloc::System, layout) }
        } else {
            std::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::GlobalAlloc::dealloc(&std::alloc::System, pointer, layout) }
    }
}
`,
  'src/macros.rs': `macro_rules! double {
    ($x:expr) => {
        $x * 2
    };
}

pub fn twice(x: u32) -> u32 {
    double!(x) + double!(1)
}
`,
  // Statements of every shape assertion assessment reads a value from: the
  // change a test would have to catch depends on the value's type.
  'src/values.rs': `use std::alloc::{alloc, dealloc, Layout};
use std::cmp::Ordering;
use std::ptr;

#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32) -> Self {
        Version { major, minor }
    }

    pub fn from_pair(pair: (u32, u32)) -> Self {
        let made = Version::new(pair.0, pair.1);
        made
    }

    pub fn with_minor(&self, minor: u32) -> Self {
        let copy = Version { minor, ..self.clone() };
        copy
    }

    pub fn order(&self, other: &Version) -> Ordering {
        let by_major = self.major.cmp(&other.major);
        if by_major != Ordering::Equal {
            return by_major;
        }
        self.minor.cmp(&other.minor)
    }
}

pub fn parse(text: &str) -> Result<Version, String> {
    let (major, minor) = text.split_once('.').ok_or("no dot")?;
    let major: u32 = major.parse().map_err(|_| "major".to_string())?;
    let minor = minor.parse::<u32>().map_err(|_| "minor".to_string())?;
    Ok(Version::new(major, minor))
}

pub fn first_or(text: &str, fallback: Version) -> Version {
    let parsed = match parse(text) {
        Ok(version) => version,
        Err(_) => fallback,
    };
    parsed
}

pub fn maybe(text: &str) -> Option<u32> {
    let found = Some(text.len() as u32);
    let doubled = found.map(|n| n * 2);
    doubled
}

pub fn pair(text: &str) -> (usize, bool) {
    let shaped = (text.len(), text.is_empty());
    shaped
}

pub fn scaled(values: &[f64], factor: f64) -> Vec<f64> {
    let scale = |value: f64| -> f64 { value * factor };
    let out = values.iter().map(|v| scale(*v)).collect::<Vec<_>>();
    out
}

pub fn bytes_sum(data: &[u8]) -> u32 {
    let start = data.as_ptr();
    let mut total = 0u32;
    for index in 0..data.len() {
        let at = unsafe { start.add(index) };
        let byte = unsafe { at.read() };
        total += u32::from(byte);
    }
    total
}

pub fn first_word(data: &[u16]) -> u16 {
    let raw = ptr::addr_of!(data[0]);
    let as_bytes = raw.cast::<u8>();
    let low = unsafe { as_bytes.read() };
    u16::from(low) | (unsafe { raw.read() } & 0xff00)
}

pub fn scratch(len: usize) -> u8 {
    let layout = Layout::from_size_align(len, 1).unwrap();
    let block = unsafe { alloc(layout) };
    unsafe { block.write(7) };
    let value = unsafe { block.read() };
    unsafe { dealloc(block, layout) };
    value
}
`,
  'tests/values.rs': `use std::cmp::Ordering;
use zoo::values::*;

#[test]
fn values_of_every_shape() {
    let one = Version::new(1, 2);
    assert_eq!(Version::from_pair((1, 2)), one);
    assert_eq!(one.with_minor(5).minor, 5);
    assert_eq!(one.order(&Version::new(2, 0)), Ordering::Less);
    assert_eq!(one.order(&Version::new(1, 1)), Ordering::Greater);
    assert_eq!(parse("3.4"), Ok(Version::new(3, 4)));
    assert!(parse("nodot").is_err());
    assert!(parse("x.1").is_err());
    assert!(parse("1.y").is_err());
    assert_eq!(first_or("bad", one.clone()), one);
    assert_eq!(first_or("2.0", one.clone()), Version::new(2, 0));
    assert_eq!(maybe("abc"), Some(6));
    assert_eq!(pair(""), (0, true));
    assert_eq!(scaled(&[1.0, 2.0], 2.0), vec![2.0, 4.0]);
    assert_eq!(bytes_sum(&[1, 2, 3]), 6);
    assert_eq!(first_word(&[0x1234]), 0x1234);
    assert_eq!(scratch(4), 7);
}
`,
  'tests/consts.rs': `use std::alloc::{GlobalAlloc, Layout};
use zoo::consts::*;

#[test]
fn compile_time_code_runs_unprobed() {
    assert_eq!(first_even(&[1, 3, 4]), Some(4));
    assert_eq!(first_even(&[0, 1]), None);
    assert_eq!((sign(5), sign(0), sign(-5)), (1, 0, -1));
    assert_eq!((halvings(0), halvings(8)), (0, 4));
    assert_eq!(SQUARES, [0, 1, 4]);
    assert_eq!(inline_const(), 2);
    assert_eq!(sized().len(), 2);
    let layout = Layout::from_size_align(8, 8).unwrap();
    unsafe {
        let block = Counting.alloc(layout);
        assert!(!block.is_null());
        Counting.dealloc(block, layout);
        assert!(Counting.alloc(Layout::from_size_align(0, 1).unwrap()).is_null());
    }
}
`,
  'tests/tail.rs': `use zoo::tail::*;

#[test]
fn inner_breaks() {
    assert_eq!(first_hit(&[vec![1, 7], vec![9]]), Some(7));
    assert_eq!(first_hit(&[vec![], vec![9]]), Some(9));
    assert_eq!(first_hit(&[vec![2]]), None);
    assert_eq!(first_hit(&[]), None);
}

#[test]
fn const_chains() {
    assert_eq!(chained(Some(3)), 3);
    assert_eq!(chained(Some(1)), 0);
    assert_eq!(chained(None), 0);
}

#[test]
fn closing_statements() {
    assert_eq!(logged(2), 2);
    assert_eq!(logged(0), 0);
    checked(5);
}

#[test]
#[should_panic]
fn a_closing_assertion_fails() {
    checked(100);
}
`,
  'tests/zoo.rs': `use zoo::consts::*;
use zoo::flow::*;
use zoo::macros::*;
use zoo::shapes::*;

#[test]
fn control_flow() {
    assert_eq!(pick(Some(5), 3), "over");
    assert_eq!(pick(Some(1), 3), "under");
    assert_eq!(pick(None, 3), "none");
    assert_eq!(sum_until(&[1, 2, 9, 4], 9), 3);
    assert_eq!(sum_until(&[1, 2], 9), 3);
    assert_eq!(find(&[vec![1, 2], vec![3]], 3), Some((1, 0)));
    assert_eq!(find(&[vec![1]], 7), None);
    assert_eq!(find(&[], 7), None);
    assert_eq!(find(&[vec![]], 7), None);
    assert_eq!(sum_until(&[], 9), 0);
    assert_eq!(classify(-1), "negative");
    assert_eq!(classify(0), "zero");
    assert_eq!(classify(5), "small");
    assert_eq!(classify(50), "large");
}

#[test]
fn early_returns() {
    assert_eq!(add(" 1", "2 "), Ok(3));
    assert!(add("x", "2").is_err());
    assert!(add("1", "y").is_err());
    assert_eq!(doubled(Some(2)), Some(4));
    assert_eq!(doubled(None), None);
}

#[test]
fn expressions() {
    assert!(is_vowel('a'));
    assert!(!is_vowel('b'));
    assert_eq!(zeros(2), vec![0, 0]);
    assert_eq!(adder(2)(1), 3);
    assert_eq!(adder(0)(1), 1);
    assert!(describe("cart", 2).contains("plural"));
    assert_eq!(describe("cart", 1), "cart has one item");
}

#[test]
fn logical_values() {
    assert!(on_axis(0, 1));
    assert!(on_axis(1, 0));
    assert!(!on_axis(1, 1));
    assert!(both(true, true));
    assert!(!both(false, true));
    assert!(!both(true, false));
    assert_eq!(in_band(&[5, -1, 20]), vec![5]);
    assert_eq!(gate(true, false, true), "open");
    assert_eq!(gate(false, true, true), "open");
    assert_eq!(gate(false, false, true), "shut");
    assert_eq!(gate(true, false, false), "shut");
}

#[test]
fn shapes_and_limits() {
    assert_eq!(Square(4.0).size(), "big");
    assert_eq!(Square(1.0).size(), "small");
    assert_eq!(magnitude(-3), 3);
    assert_eq!(clamp(100), LIMIT);
    assert_eq!(clamp(1), 1);
    assert_eq!(lookup(0), Some(1));
    assert_eq!(lookup(9), None);
    assert_eq!(twice(3), 8);
}
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  const plain = run('cargo', ['test', '-q']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', 'cargo', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  // Each construct was driven both ways, so nothing is left uncovered.
  for (const file of ['src/flow.rs', 'src/shapes.rs', 'src/tail.rs']) {
    const entry = byFile[file];
    assert.ok(entry, `${file} was not measured\n${report}`);
    for (const metric of ['lines', 'branches', 'mcdcConditions']) {
      assert.equal(entry.covered[metric], entry.totals[metric], `${file}: ${metric}\n${report}`);
    }
    assert.ok(entry.totals.lines > 0, `${file} has no lines\n${report}`);
  }
  // Const contexts and macro expansions cannot hold a probe, and say so.
  assert.ok(byFile['src/macros.rs']?.measurementLimitations >= 1, report);
  assert.equal(byFile['src/tail.rs'].measurementLimitations, 2, report);
  // Code that runs at compile time or in the allocator is declared unmeasured,
  // never owed: an array length read as a decision nothing evaluated would be
  // a gap no test could close.
  const consts = byFile['src/consts.rs'];
  assert.ok(consts?.measurementLimitations >= 2, report);
  for (const metric of ['lines', 'branches', 'mcdcConditions']) {
    assert.equal(consts.covered[metric], consts.totals[metric], `src/consts.rs: ${metric}\n${report}`);
  }
  // Every construct is read for what a test would have to catch, too: each
  // executed statement asked about and, with a stand-in that says yes, asserted.
  const assessed = await assessWithStandIn(binary, root);
  assert.equal(assessed.status, 0, assessed.output);
  assert.match(assessed.output, /\b100% asserted \((\d+) of \1 executed statements\)/, assessed.output);
  console.log('[rust-syntax-zoo] every construct the Rust instrumenter rewrites reads as its tests drove it, and is assessed');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
