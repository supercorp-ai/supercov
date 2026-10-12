// What a failing test prints is what it prints without Supercov: the same
// file, line and column under every failure, and no frame of Supercov's.
//
//   node scripts/failure-output.mjs python ruby go rust node ava
//
// Each language gets the same small project: a function that refuses a short
// key, a test that meets the refusal, and a test whose comparison fails. Its
// suite runs once alone and once under Supercov, and what the two print is
// compared line for line, with only what a clock or an address writes taken
// out. Before this, on 4.3.0:
//   - Ruby: Minitest named a failure `[.supercov/…/supercov_runtime.rb:818]`;
//     RSpec and test-unit listed the runtime's frames under the test's.
//   - Go: a panic named the copy's file, at `crypto.go:24` for line 23, and
//     `./...` listed Supercov's runtime package.
//   - Rust: a panic was reported at `src/lib.rs:9:84` for `8:9`, each failed
//     test had a report of its own and a test that passed no line at all.
//   - Python: unittest printed its wrapper's frames, and its own, over the
//     test's line.
//   - node:test: a failing test had no frame of its own file, and a rewritten
//     module was named by its path where Node names a module by its URL.
//   - AVA: a frame of the test file named Supercov's copy.
// pytest already read the same. A language whose toolchain is not installed
// is refused, not skipped: a gate that runs nothing proves nothing.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);

function project(files) {
  // The directory's own name: on Windows the temporary directory is given by
  // its short name (`RUNNER~1`), which a run would print where Supercov, that
  // resolves the project, prints the long one.
  const root = resolve(mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-failure-output-')), 'shop');
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  spawnSync('git', ['init', '-q'], { cwd: root });
  return root;
}

// What a run printed, without what differs between any two runs.
function printed(root, command, args, tidy, { passes = false } = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    env: { ...process.env, NO_COLOR: '1', CI: '1' },
  });
  assert.equal(result.error, undefined, `${command} did not start: ${result.error}`);
  if (passes) assert.equal(result.status, 0, `${command} ${args.join(' ')} failed:\n${result.stdout}${result.stderr}`);
  else assert.notEqual(result.status, 0, `${command} ${args.join(' ')} passed:\n${result.stdout}${result.stderr}`);
  return `${result.stdout}${result.stderr}`
    .split('\n')
    .map((line) => tidy(line.trimEnd()))
    .filter((line) => line !== '' && !/^\[(?:supercov|coverage)\]/u.test(line));
}

function compare(name, root, command, args, { tidy = (line) => line, expects }) {
  const plain = printed(root, command, args, tidy);
  for (const pattern of expects) assert.ok(plain.some((line) => pattern.test(line)), `${name}: ${pattern}\n${plain.join('\n')}`);
  const measured = printed(root, binary, ['--', command, ...args], tidy);
  assert.deepEqual(measured, plain, name);
  console.log(`[failure-output] ${name} reads as it does without Supercov`);
}

// An interpreter that has pytest: `PYTHON` when it names one, or a virtual
// environment made from the interpreter the Python gate measures
// (`SUPERCOV_PYTHON`, or the newest CPython on PATH).
function pythonWithPytest() {
  if (process.env.PYTHON) return process.env.PYTHON;
  const candidates = process.env.SUPERCOV_PYTHON
    ? [process.env.SUPERCOV_PYTHON]
    : ['python3.14', 'python3.13', 'python3.12', 'python3.11', 'python3.10', 'python3.9', 'python3', 'python'];
  const base = candidates.find((candidate) => spawnSync(candidate, ['-c', 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)']).status === 0);
  assert.ok(base, 'failure-output needs CPython 3.9 or newer on PATH (or SUPERCOV_PYTHON)');
  const venv = resolve(mkdtempSync(resolve(tmpdir(), 'supercov-failure-output-venv-')), 'venv');
  const made = spawnSync(base, ['-m', 'venv', venv], { encoding: 'utf8' });
  assert.equal(made.status, 0, made.stderr);
  const python = resolve(venv, process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python');
  const installed = spawnSync(python, ['-m', 'pip', 'install', '--quiet', '--disable-pip-version-check', 'pytest'], { encoding: 'utf8' });
  assert.equal(installed.status, 0, `installing pytest: ${installed.stdout}\n${installed.stderr}`);
  return python;
}

const languages = {
  python() {
    const root = project({
      'shop/__init__.py': '',
      'shop/crypto.py': 'def seal(key, body):\n    if len(key) < 4:\n        raise ValueError("key too short")\n    return {"key": key, "body": body.upper()}\n',
      'tests/test_crypto.py': 'from shop.crypto import seal\n\n\ndef test_refuses_a_short_key():\n    sealed = seal("ab", "x")\n    assert sealed["body"] == "X"\n\n\ndef test_compares():\n    assert seal("abcd", "y")["body"] == "Z"\n',
      'pytest.ini': '[pytest]\ntestpaths = tests\n',
    });
    compare('pytest', root, pythonWithPytest(), ['-m', 'pytest', '-q', '-p', 'no:cacheprovider'], {
      tidy: (line) => line.replace(/ in [\d.]+s/u, ''),
      expects: [/^shop[\\/]crypto\.py:3: ValueError$/u, /^tests[\\/]test_crypto\.py:10: AssertionError$/u],
    });
    const unit = project({
      'shop/__init__.py': '',
      'shop/crypto.py': 'def seal(key, body):\n    if len(key) < 4:\n        raise ValueError("key too short")\n    return {"key": key, "body": body.upper()}\n',
      'tests/__init__.py': '',
      'tests/test_unit.py': 'import unittest\n\nfrom shop.crypto import seal\n\n\nclass CryptoTest(unittest.TestCase):\n    def test_refuses_a_short_key(self):\n        sealed = seal("ab", "x")\n        self.assertEqual(sealed["body"], "X")\n\n    def test_compares(self):\n        self.assertEqual(seal("abcd", "y")["body"], "Z")\n',
    });
    compare('unittest', unit, pythonWithPytest(), ['-m', 'unittest', 'tests.test_unit'], {
      tidy: (line) => line.replace(/ in [\d.]+s/u, ''),
      expects: [/test_unit\.py", line 12, in test_compares$/u, /crypto\.py", line 3, in seal$/u],
    });
  },
  ruby() {
    const ruby = process.env.RUBY ?? 'ruby';
    const source = 'module Crypto\n  def self.seal(key, body)\n    if key.length < 4\n      raise ArgumentError, "key too short"\n    end\n    { key: key, body: body.upcase }\n  end\nend\n';
    const minitest = project({
      'lib/crypto.rb': source,
      'test/crypto_test.rb': 'require "minitest/autorun"\nrequire_relative "../lib/crypto"\n\nclass CryptoTest < Minitest::Test\n  def test_refuses_a_short_key\n    sealed = Crypto.seal("ab", "x")\n    assert_equal "X", sealed[:body]\n  end\n\n  def test_compares\n    assert_equal "Z", Crypto.seal("abcd", "y")[:body]\n  end\nend\n',
    });
    compare('Minitest', minitest, ruby, ['-Itest', 'test/crypto_test.rb', '--seed', '1'], {
      tidy: (line) => (line.startsWith('Finished in') ? '' : line),
      expects: [/^CryptoTest#test_compares \[test\/crypto_test\.rb:11\]:$/u],
    });
    // Asked of RubyGems and not by requiring it: test-unit runs when loaded.
    const available = (gem) => spawnSync(ruby, ['-e', `exit(Gem::Specification.find_all_by_name("${gem}").empty? ? 1 : 0)`]).status === 0;
    if (available('rspec-core')) {
      const rspec = project({
        'lib/crypto.rb': source,
        'spec/crypto_spec.rb': 'require_relative "../lib/crypto"\n\nRSpec.describe Crypto do\n  it "refuses a short key" do\n    sealed = Crypto.seal("ab", "x")\n    expect(sealed[:body]).to eq("X")\n  end\n\n  it "compares" do\n    expect(Crypto.seal("abcd", "y")[:body]).to eq("Z")\n  end\nend\n',
      });
      compare('RSpec', rspec, ruby, ['-e', 'load Gem.bin_path("rspec-core", "rspec")', '--', 'spec', '--seed', '1', '--no-color'], {
        tidy: (line) => (line.startsWith('Finished in') ? '' : line),
        expects: [/^\s+# \.\/spec\/crypto_spec\.rb:10:in /u],
      });
    }
    if (available('test-unit')) {
      const unit = project({
        'lib/crypto.rb': source,
        'test/crypto_unit_test.rb': 'require "test/unit"\nrequire_relative "../lib/crypto"\n\nclass CryptoUnit < Test::Unit::TestCase\n  def test_refuses_a_short_key\n    puts "run from #{caller.grep(/test-?suite(?:-runner)?\\.rb/).first.to_s[/[\\w-]+\\.rb:\\d+/]}"\n    sealed = Crypto.seal("ab", "x")\n    assert_equal "X", sealed[:body]\n  end\n\n  def test_compares\n    assert_equal "Z", Crypto.seal("abcd", "y")[:body]\n  end\nend\n',
      });
      compare('test-unit', unit, ruby, ['test/crypto_unit_test.rb', '--no-use-color'], {
        tidy: (line) => (/^Finished in|tests\/s/u.test(line) ? '' : line),
        // test-unit reads the parameters of a test's `run` to tell one a
        // script overrode for an older release, and calls that one another
        // way. Supercov's `run(*args)` read as such an override. The file
        // that calls a test is `testsuite.rb` in older releases.
        expects: [/^test\/crypto_unit_test\.rb:12:in /u, /run from test-?suite(?:-runner)?\.rb:\d+$/u],
      });
    }
  },
  go() {
    const root = project({
      'go.mod': 'module example.com/shop\n\ngo 1.22\n',
      // Switches with no `default` first, one of them on one line and one
      // with no clause: the clause Supercov adds to each took three lines, and
      // everything below was reported three lines late for each.
      'crypto.go': 'package shop\n\nimport (\n\t"errors"\n\t"strings"\n)\n\nfunc Kind(key string) int {\n\tswitch {\n\tcase len(key) == 0:\n\t\treturn 0\n\tcase len(key) < 4:\n\t\treturn 1\n\t}\n\tswitch len(key) { case 4: return 2 }\n\tswitch key {\n\t}\n\treturn 3\n}\n\nfunc Seal(key, body string) (string, error) {\n\tif len(key) < 4 {\n\t\treturn "", errors.New("key too short")\n\t}\n\treturn strings.ToUpper(body), nil\n}\n\nfunc MustSeal(key, body string) string {\n\tsealed, err := Seal(key, body)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\treturn sealed\n}\n',
      'crypto_test.go': 'package shop\n\nimport "testing"\n\nfunc TestSealsABody(t *testing.T) {\n\tif got := MustSeal("abcd", "x"); got != "X" {\n\t\tt.Fatalf("got %q", got)\n\t}\n}\n\nfunc TestCompares(t *testing.T) {\n\tif got := MustSeal("abcd", "y"); got != "Z" {\n\t\tt.Errorf("got %q, want %q", got, "Z")\n\t}\n}\n\nfunc TestRefusesAShortKey(t *testing.T) {\n\tsealed := MustSeal("ab", "x")\n\tif sealed != "X" {\n\t\tt.Fatalf("got %q", sealed)\n\t}\n}\n',
    });
    compare('go test', root, 'go', ['test', './...'], {
      // Addresses, goroutine numbers and times differ between any two runs.
      // A function Go would have inlined is called under Supercov, so its
      // frame shows its arguments where the inlined one shows `(...)`.
      tidy: (line) =>
        line
          .replace(/0x[0-9a-f]+\??/gu, '0x')
          .replace(/goroutine \d+/u, 'goroutine')
          .replace(/\(\{0x[^)]*\)|\(0x(?:, 0x)*\)|\(\.\.\.\)/u, '()')
          .replace(/\s+\+0x$/u, '')
          .replace(/\s[\d.]+s$/u, '')
          .replace(/\(\d+\.\d+s\)/u, ''),
      expects: [/^\s+crypto_test\.go:13: got "Y", want "Z"$/u, /[\\/]crypto\.go:31$/u, /[\\/]crypto_test\.go:18$/u],
    });
    // And what the command writes is in the project when it ends: the
    // coverage profile stayed in Supercov's copy, and a step after the
    // tests that reads it found none.
    const profiled = spawnSync(binary, ['--', 'go', 'test', '-coverprofile=cover.out', '-run', 'TestSealsABody', './...'], { cwd: root, encoding: 'utf8' });
    assert.equal(profiled.status, 0, `${profiled.stdout}${profiled.stderr}`);
    assert.match(readFileSync(resolve(root, 'cover.out'), 'utf8'), /^mode: set\nexample\.com\/shop\/crypto\.go:8\./u);
    console.log('[failure-output] go test leaves its coverage profile in the project');
  },
  rust() {
    const root = project({
      'Cargo.toml': '[package]\nname = "shop"\nversion = "0.1.0"\nedition = "2021"\n',
      // A `match` and a loop first: their probes took lines of their own, and
      // every panic and doctest below them was reported two lines down for
      // each (arrayvec's doctest at line 302 read `(line 304)`).
      'src/lib.rs': 'pub fn kind(key: &str) -> u8 {\n    match key.len() {\n        0 => 0,\n        n if n < 4 => {\n            1\n        }\n        _ => 2,\n    }\n}\n\npub fn scan(key: &str, limit: usize) -> usize {\n    let mut seen = 0;\n    while seen < limit && seen < key.len() {\n        if key.as_bytes()[seen] == b\'-\' || seen > 8 {\n            break;\n        } else {\n            seen += 1;\n        }\n    }\n    seen\n}\n\n/// Seals a body.\n///\n/// ```\n/// assert_eq!(shop::seal("abcd", "x"), "X");\n/// ```\n///\n/// ```\n/// assert_eq!(shop::seal("abcd", "y"), "Z");\n/// ```\npub fn seal(key: &str, body: &str) -> String {\n    if key.len() < 4 {\n        panic!("key too short");\n    }\n    body.to_uppercase()\n}\n\npub fn first(key: &str) -> char {\n    key.chars().next().unwrap()\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn seals_a_body() {\n        assert_eq!(seal("abcd", "x"), "X");\n        assert_eq!(kind("abcd"), 2);\n        assert_eq!(scan("ab-cd", 9), 2);\n    }\n\n    #[test]\n    fn refuses_a_short_key() {\n        let sealed = seal("ab", "x");\n        assert_eq!(sealed, "X");\n    }\n\n    #[test]\n    fn compares() {\n        assert_eq!(seal("abcd", "y"), "Z");\n    }\n\n    #[test]\n    #[ignore = "slow"]\n    fn is_slow() {}\n}\n',
      'tests/outside.rs': '#[test]\nfn seals_from_outside() {\n    assert_eq!(shop::seal("abcd", "q"), "Q");\n}\n',
    });
    // Cargo writes its own lines on one stream and libtest's on the other, and
    // libtest lists tests as they finish, so the two runs are compared as the
    // lines each printed and not in an order neither has. Before this a run
    // under Supercov had a report for each failed test (`running 1 test`,
    // `2 filtered out`), no line for a test that passed and none of what a
    // failed doctest printed.
    const tidy = (line) => line
      .replace(/\(\d+\) panicked/u, 'panicked')
      .replace(/-[0-9a-f]{16}(?:\.exe)?\)$/u, '-HASH)')
      .replace(/finished in [\d.]+s$/u, 'finished');
    const lines = (command, args, outcome) => printed(root, command, args, tidy, outcome)
      .filter((line) => !/^\s+(?:Compiling|Finished|Blocking|Locking)\b|^error: |^\s+`--/u.test(line))
      .sort();
    const plain = lines('cargo', ['test', '--no-fail-fast']);
    for (const expected of [
      // rustc names the file with the platform's separator.
      `thread 'tests::refuses_a_short_key' panicked at src${sep}lib.rs:34:9:`,
      `thread 'tests::compares' panicked at src${sep}lib.rs:62:9:`,
      'running 4 tests',
      'test tests::seals_a_body ... ok',
      'test tests::is_slow ... ignored, slow',
      'test result: FAILED. 1 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished',
      '   Doc-tests shop',
    ]) assert.ok(plain.includes(expected), `cargo test: ${expected}\n${plain.join('\n')}`);
    assert.deepEqual(lines(binary, ['--', 'cargo', 'test', '--no-fail-fast']), plain, 'cargo test');
    // The tests marked `#[ignore]` alone: two binaries and the doctests each
    // report that they ran none of theirs. This failed outright, on "the pass
    // for doctest … ran 0 doctest(s) instead of that one alone".
    const ignored = lines('cargo', ['test', '--', '--ignored'], { passes: true });
    for (const expected of [
      'running 1 test',
      'test tests::is_slow ... ok',
      'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished',
    ]) assert.ok(ignored.includes(expected), `cargo test -- --ignored: ${expected}\n${ignored.join('\n')}`);
    assert.deepEqual(lines(binary, ['--', 'cargo', 'test', '--', '--ignored'], { passes: true }), ignored, 'cargo test -- --ignored');
    console.log('[failure-output] cargo test reads as it does without Supercov');
  },
};

const moduleSource = 'export function seal(key, body) {\n  if (key.length < 4) {\n    throw new Error("key too short");\n  }\n  return { key, body: body.toUpperCase() };\n}\n';
languages.node = () => {
  const root = project({
    'package.json': '{ "name": "shop", "private": true, "type": "module" }\n',
    'src/crypto.js': moduleSource,
    'test/crypto.test.js': 'import test from "node:test";\nimport assert from "node:assert/strict";\nimport { seal } from "../src/crypto.js";\n\ntest("refuses a short key", () => {\n  const sealed = seal("ab", "x");\n  assert.equal(sealed.body, "X");\n});\n\ntest("compares", () => {\n  assert.equal(seal("abcd", "y").body, "Z");\n});\n\ntest("fails later", async () => {\n  await new Promise((settle) => setTimeout(settle, 1));\n  assert.deepEqual(seal("abcd", "z"), { key: "abcd", body: "z" });\n});\n',
  });
  compare('node:test', root, process.execPath, ['--test', '--test-reporter=spec', 'test/crypto.test.js'], {
    tidy: (line) => line.replace(/\(\d+(?:\.\d+)?ms\)/u, '').replace(/duration_ms [\d.]+/u, 'duration_ms'),
    expects: [/^\s+at seal \(file:\/\/.*\/src\/crypto\.js:3:11\)$/u, /^\s+at TestContext\.<anonymous> \(file:\/\/.*\/test\/crypto\.test\.js:11:10\)$/u],
  });
};
// AVA is the repository's own; a project reaches it as an installed one would.
languages.ava = () => {
  const root = project({
    'package.json': '{ "name": "shop", "private": true, "type": "module" }\n',
    '.gitignore': 'node_modules\n.supercov\n',
    'src/crypto.js': moduleSource,
    'test/crypto.test.js': 'import test from "ava";\nimport { seal } from "../src/crypto.js";\n\ntest("refuses a short key", (t) => {\n  const sealed = seal("ab", "x");\n  t.is(sealed.body, "X");\n});\n\ntest("compares", (t) => {\n  t.is(seal("abcd", "y").body, "Z");\n});\n',
  });
  symlinkSync(resolve(repository, 'node_modules'), resolve(root, 'node_modules'), 'junction');
  compare('AVA', root, process.execPath, [resolve(root, 'node_modules/ava/entrypoints/cli.js'), '--serial'], {
    tidy: (line) => line.replace(/\(\d+(?:\.\d+)?m?s\)/u, ''),
    expects: [/^\s+at seal \(file:\/\/.*\/src\/crypto\.js:3:11\)$/u, /file:\/\/test\/crypto\.test\.js:10:5$/u],
  });
};

const asked = process.argv.slice(2);
assert.ok(asked.length > 0 && asked.every((name) => languages[name]), `usage: failure-output.mjs ${Object.keys(languages).join(' ')}`);
for (const name of asked) languages[name]();
