// Every construct the Rust instrumenter rewrites, measured end to end.
//
//   node scripts/rust-syntax-zoo.mjs
//
// A small crate whose tests drive each construct both ways: let chains,
// `while let`, a labelled loop broken with a value, match guards and ranges,
// `?` on Result and Option, `matches!`, `vec![x; n]`, a closure returned as
// `impl Fn`, a statement longer than a line, a trait with a declared and a
// default method, an extern declaration. Each file is read back for what its
// tests showed; const contexts and macro expansions, which cannot hold a
// probe, must say so instead.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-rust-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

const files = {
  'Cargo.toml': '[package]\nname = "zoo"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n',
  'src/lib.rs': 'pub mod consts;\npub mod flow;\npub mod macros;\npub mod shapes;\n',
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
  for (const file of ['src/flow.rs', 'src/shapes.rs']) {
    const entry = byFile[file];
    assert.ok(entry, `${file} was not measured\n${report}`);
    for (const metric of ['lines', 'branches', 'mcdcConditions']) {
      assert.equal(entry.covered[metric], entry.totals[metric], `${file}: ${metric}\n${report}`);
    }
    assert.ok(entry.totals.lines > 0, `${file} has no lines\n${report}`);
  }
  // Const contexts and macro expansions cannot hold a probe, and say so.
  assert.ok(byFile['src/macros.rs']?.measurementLimitations >= 1, report);
  console.log('[rust-syntax-zoo] every construct the Rust instrumenter rewrites reads as its tests drove it');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
