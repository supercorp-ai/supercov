// A program that hands its process over to another keeps what it ran.
//
//   node scripts/exec-handover.mjs
//
// A cache front end that restores a key and then `exec`s the runner's Node
// replaces its own process: no exit handler runs, so evidence a runtime
// writes at exit went with it, and every line it ran read as never run. For
// each language a test spawns such a program and the line it ran must be
// covered by that test.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(tmpdir(), `supercov-exec-handover-${process.pid}-${Date.now()}`);
const run = (cwd, command, args) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1' } });
const available = (command, args) => run(repository, command, args).status === 0;

const cases = [
  {
    language: 'JavaScript',
    // process.execve is Node 23.11 and newer.
    available: typeof process.execve === 'function',
    command: ['node', '--test'],
    line: 'src/cache.js:3',
    test: 'a cached key is a hit',
    files: {
      'package.json': '{ "name": "exec-js", "private": true, "type": "module" }\n',
      'src/cache.js': 'export function restore(key) {\n  if (key.startsWith("v1-")) {\n    return "hit";\n  }\n  return "miss";\n}\n',
      'src/cli.js': 'import { restore } from "./cache.js";\nconst outcome = restore(process.argv[2]);\nprocess.execve(process.execPath, [process.execPath, "-e", `console.log(${JSON.stringify(outcome)})`]);\n',
      'test/cli.test.js': 'import test from "node:test";\nimport assert from "node:assert/strict";\nimport { execFileSync } from "node:child_process";\ntest("a cached key is a hit", () => {\n  assert.equal(execFileSync(process.execPath, ["src/cli.js", "v1-abc"], { encoding: "utf8" }).trim(), "hit");\n});\n',
    },
  },
  {
    language: 'Ruby',
    available: available('ruby', ['-e', 'exit(RUBY_VERSION >= "3.3" ? 0 : 1)']),
    command: ['ruby', 'test/cli_test.rb'],
    line: 'lib/cache.rb:4',
    test: 'CliTest#test_a_cached_key_is_a_hit',
    files: {
      'lib/cache.rb': 'module Cache\n  def self.restore(key)\n    if key.start_with?("v1-")\n      "hit"\n    else\n      "miss"\n    end\n  end\nend\n',
      'bin/cli.rb': 'require_relative "../lib/cache"\noutcome = Cache.restore(ARGV[0])\nexec(RbConfig.ruby, "-e", "puts #{outcome.inspect}")\n',
      'test/cli_test.rb': 'require "minitest/autorun"\nrequire "open3"\nrequire "rbconfig"\nclass CliTest < Minitest::Test\n  def test_a_cached_key_is_a_hit\n    out, status = Open3.capture2(RbConfig.ruby, "bin/cli.rb", "v1-abc")\n    assert status.success?\n    assert_equal "hit", out.strip\n  end\nend\n',
    },
  },
  {
    language: 'Python',
    available: available('python3', ['-c', 'import pytest']),
    command: ['python3', '-m', 'pytest', '-q'],
    line: 'app/cache.py:3',
    test: 'test_a_cached_key_is_a_hit',
    files: {
      'pyproject.toml': '[project]\nname = "exec-py"\nversion = "0.1.0"\n',
      'app/__init__.py': '',
      'app/cache.py': 'def restore(key):\n    if key.startswith("v1-"):\n        return "hit"\n    return "miss"\n',
      'app/cli.py': 'import os\nimport sys\n\nfrom app.cache import restore\n\noutcome = restore(sys.argv[1])\nos.execv(sys.executable, [sys.executable, "-c", f"print({outcome!r})"])\n',
      'tests/test_cli.py': 'import subprocess\nimport sys\n\n\ndef test_a_cached_key_is_a_hit():\n    out = subprocess.run([sys.executable, "-m", "app.cli", "v1-abc"], capture_output=True, text=True, check=True)\n    assert out.stdout.strip() == "hit"\n',
    },
  },
];

const measured = [];
try {
  for (const entry of cases) {
    if (!entry.available) {
      console.log(`[exec-handover] ${entry.language}: skipped, its toolchain is not here`);
      continue;
    }
    const cwd = resolve(root, entry.language.toLowerCase());
    for (const [file, text] of Object.entries(entry.files)) {
      mkdirSync(dirname(resolve(cwd, file)), { recursive: true });
      writeFileSync(resolve(cwd, file), text);
    }
    assert.equal(run(cwd, 'git', ['init', '-q']).status, 0);
    const result = run(cwd, binary, ['--', ...entry.command]);
    assert.equal(result.status, 0, `${entry.language}\n${result.stdout}${result.stderr}`);
    const line = run(cwd, binary, ['runs', 'latest', 'line', entry.line]).stdout;
    assert.match(line, /Status\n {2}COVERED/u, `${entry.language}: what the program ran before exec was lost\n${line}`);
    assert.ok(line.includes(entry.test), `${entry.language}: credited to the test that started it\n${line}`);
    measured.push(entry.language);
  }
  assert.ok(measured.length > 0, 'no language could be checked here');
  console.log(`[exec-handover] ${measured.join(', ')}: what a program ran before exec is kept and credited to its test`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
