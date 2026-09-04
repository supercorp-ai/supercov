#!/usr/bin/env node
// End-to-end gate for the owned Ruby frontend: a real Ruby 3.3+ runs the
// construct fixture through RSpec and Minitest with nothing but Supercov's
// environment variables, and the published run must report the exact
// denominator and observations the fixture is designed to produce.

import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, 'target/debug/supercov');
const launcher = resolve(repository, 'bin/supercov.js');
const fixture = resolve(repository, 'tests/fixtures/ruby-coverage');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-ruby-coverage-'));
const project = resolve(temporary, 'project');

function interpreterVersion(program) {
  const probe = spawnSync(program, ['-e', 'puts RUBY_VERSION'], { encoding: 'utf8' });
  if (probe.status !== 0) return null;
  const [major, minor] = probe.stdout.trim().split('.').map(Number);
  return { major, minor };
}

function findInterpreter() {
  const candidates = process.env.SUPERCOV_RUBY
    ? [process.env.SUPERCOV_RUBY]
    : ['/opt/homebrew/opt/ruby/bin/ruby', 'ruby'];
  for (const candidate of candidates) {
    const version = interpreterVersion(candidate);
    if (version && (version.major > 3 || (version.major === 3 && version.minor >= 3))) return candidate;
  }
  throw new Error('ruby-coverage integration needs Ruby 3.3 or newer on PATH (or SUPERCOV_RUBY)');
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  assert.equal(result.status, 0, `${program} ${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  return result;
}

function supercov(args, environment) {
  return spawnSync(process.execPath, [launcher, ...args], {
    cwd: project,
    encoding: 'utf8',
    env: environment,
  });
}

function query(args, environment) {
  const result = supercov([...args, '--json'], environment);
  assert.equal(result.status, 0, result.stderr);
  const payload = JSON.parse(result.stdout);
  assert.equal(payload.ok, true, result.stdout);
  return payload.data;
}

function assertStdlibOnlyTotals(summary) {
  // Ruby 3.3 measures through Coverage alone: lines, methods and stdlib
  // branches are exact, probe-driven obligations are declared unmeasured.
  assert.equal(summary.model.variant, 'ruby-owned-coverage');
  // `case ... in`, `x = begin`, `kind = case`, `detail = {`, bare `begin` and
  // `if false` lines carry no line event on 3.3, so their statements are
  // declared unmeasured and the lines stay uncovered; on 3.4+ the runtime
  // probes them instead. Declared obligations leave the obligation totals but
  // not yet the line totals, which is how every frontend behaves today.
  assert.deepEqual([summary.coverage.lines.covered, summary.coverage.lines.total], [77, 88], JSON.stringify(summary.coverage));
  assert.equal(summary.testExitCode, 0);
}

function assertFixtureTotals(summary) {
  // The fixture is designed so `return "unreachable"` and `when Array` never
  // run, `c` in `a && (b || c)` never short-circuits, `cache[key] ||=` never
  // short-circuits, and the guard `n > 5` is never false. Everything
  // else is observed exactly, including elsif, ternaries, `for`/`while`/
  // `until`, safe navigation, case/in, rescue flow and same-line statements.
  assert.equal(summary.model.variant, 'ruby-owned-coverage');
  assert.deepEqual([summary.coverage.lines.covered, summary.coverage.lines.total], [85, 88], JSON.stringify(summary.coverage));
  assert.deepEqual([summary.coverage.branches.covered, summary.coverage.branches.total], [114, 132], JSON.stringify(summary.coverage));
  assert.deepEqual([summary.coverage.coveredConditions, summary.coverage.conditions], [13, 19], JSON.stringify(summary.coverage));
  assert.equal(summary.testExitCode, 0);
}

try {
  const ruby = findInterpreter();
  const version = interpreterVersion(ruby);
  const probes = version.major > 3 || version.minor >= 4;
  const assertTotals = probes ? assertFixtureTotals : assertStdlibOnlyTotals;
  const rubyDirectory = resolve(ruby, '..');
  const gems = resolve(temporary, 'gems');
  run(resolve(rubyDirectory, 'gem'), ['install', '--install-dir', gems, '--no-document', 'rspec', 'minitest', 'test-unit', 'cucumber']);
  cpSync(fixture, project, { recursive: true });
  run('git', ['init', '-q', '.'], { cwd: project });

  const environment = {
    ...process.env,
    SUPERCOV_RUST_BINARY: binary,
    GEM_PATH: gems,
    PATH: [rubyDirectory, resolve(gems, 'bin'), process.env.PATH].join(delimiter),
  };
  delete environment.RUBYOPT;
  delete environment.GEM_HOME;

  const rspec = supercov(['--', 'rspec'], environment);
  assert.equal(rspec.status, 0, `${rspec.stdout}\n${rspec.stderr}`);
  assert.match(rspec.stdout, /\[coverage\] evidence:/);
  assert.match(rspec.stderr, /12 test\(s\) across 1 source file\(s\)/);
  assert.match(rspec.stderr, /interpreter process\(es\) on Ruby (3\.[3-9]|[4-9])/);
  assertTotals(query(['runs', 'latest'], environment));

  if (probes) {
    const compound = query(['runs', 'latest', 'decision', 'lib/shapes.rb:8'], environment);
    assert.deepEqual(compound.decisions[0].meta.conditions, ['a', 'b', 'c']);
    const vectors = compound.decisions[0].vectors
      .map((vector) => `${vector.values.map((value) => (value === null ? '-' : value ? 'T' : 'F')).join('')}->${vector.outcome ? 'T' : 'F'}`)
      .sort();
    assert.deepEqual(vectors, ['F--->F', 'TFF->F', 'TFT->T'], 'MC/DC vectors from operand probes');

    const child = query(['runs', 'latest', 'line', 'lib/shapes.rb:8'], environment);
    assert.match(JSON.stringify(child), /shapes_spec\.rb\[1:10\]/, 'IO.popen child inherits the exact example identity');

    const rescue = query(['runs', 'latest', 'line', 'lib/shapes.rb:66'], environment);
    assert.doesNotMatch(JSON.stringify(rescue), /not observed: body completed/, 'begin completion is observed through the probe');
  } else {
    const file = query(['runs', 'latest', 'file', 'lib/shapes.rb'], environment);
    assert.match(JSON.stringify(file), /ruby-probe-obligations-need-3\.4/, 'Ruby 3.3 declares probe obligations unmeasured');
  }

  if (probes) {
    // A file Supercov cannot instrument, or is asked to leave alone, still
    // loads and is still measured through Ruby's Coverage module; only what a
    // probe would have proven is declared. This is the same path a compile
    // failure takes.
    const skipped = supercov(['--', 'rspec'], { ...environment, SUPERCOV_RUBY_SKIP_PROBES: 'lib/shapes.rb' });
    assert.equal(skipped.status, 0, `${skipped.stdout}\n${skipped.stderr}`);
    assert.match(skipped.stderr, /12 test\(s\) across 1 source file\(s\)/, 'the suite still runs unmodified');
    const stdlibOnly = query(['runs', 'latest'], environment);
    assert.deepEqual(
      [stdlibOnly.coverage.lines.covered, stdlibOnly.coverage.lines.total],
      [80, 88],
      JSON.stringify(stdlibOnly.coverage),
    );
    assert.deepEqual(
      [stdlibOnly.coverage.branches.covered, stdlibOnly.coverage.branches.total],
      [53, 60],
      JSON.stringify(stdlibOnly.coverage),
    );
    assert.deepEqual(
      [stdlibOnly.coverage.functions.covered, stdlibOnly.coverage.functions.total],
      [18, 18],
      'methods stay measured without probes',
    );
    const declared = query(['runs', 'latest', 'file', 'lib/shapes.rb'], environment);
    assert.ok(declared.totalLimitations > 0, 'probe-only obligations are declared, not reported as gaps');
    assert.match(JSON.stringify(declared), /ruby-file-not-instrumented/);
  }

  const minitest = supercov(['--', 'ruby', '-Itest', 'test/shapes_test.rb'], environment);
  assert.equal(minitest.status, 0, `${minitest.stdout}\n${minitest.stderr}`);
  assert.match(minitest.stderr, /3 test\(s\) across 1 source file\(s\)/);
  const runners = supercov(['runs', 'latest', 'runners'], environment);
  assert.match(runners.stdout, /minitest\s+3 test\(s\)/);
  const matcher = query(['runs', 'latest', 'line', 'lib/shapes.rb:51'], environment);
  assert.match(JSON.stringify(matcher), /ShapesTest#test_matcher/, 'Minitest identity reaches the line');

  const testUnit = supercov(['--', 'ruby', '-Itest', 'test/unit_style_test.rb'], environment);
  assert.equal(testUnit.status, 0, `${testUnit.stdout}\n${testUnit.stderr}`);
  assert.match(testUnit.stderr, /2 test\(s\) across 1 source file\(s\)/);
  const testUnitRunners = supercov(['runs', 'latest', 'runners'], environment);
  assert.match(testUnitRunners.stdout, /test-unit\s+2 test\(s\)/);
  const negation = query(['runs', 'latest', 'line', 'lib/shapes.rb:36'], environment);
  assert.match(JSON.stringify(negation), /UnitStyleTest#test_negation/, 'test-unit identity reaches the line');

  // Thread-parallel Minitest: probes stay per test, stdlib deltas that
  // overlapped go to the run and the limitation says so.
  const parallel = supercov(['--', 'ruby', '-Itest', 'test/parallel_test.rb'], environment);
  assert.equal(parallel.status, 0, `${parallel.stdout}\n${parallel.stderr}`);
  assert.match(parallel.stderr, /4 test\(s\) across 1 source file\(s\)/);
  const parallelFile = query(['runs', 'latest', 'file', 'lib/shapes.rb'], environment);
  assert.match(JSON.stringify(parallelFile), /ruby-concurrent-test-phases/, 'thread-parallel run declares its limitation');

  const cucumber = supercov(['--', 'cucumber', '--publish-quiet'], environment);
  assert.equal(cucumber.status, 0, `${cucumber.stdout}\n${cucumber.stderr}`);
  assert.match(cucumber.stderr, /2 test\(s\) across 1 source file\(s\)/);
  const cucumberRunners = supercov(['runs', 'latest', 'runners'], environment);
  assert.match(cucumberRunners.stdout, /cucumber\s+2 test\(s\)/);
  const countdown = query(['runs', 'latest', 'line', 'lib/shapes.rb:99'], environment);
  assert.match(JSON.stringify(countdown), /features\/shapes\.feature:7/, 'Cucumber scenario identity reaches the line');

  console.log(`[ruby-coverage] ${ruby} measured the fixture through RSpec, Minitest, test-unit, Cucumber and thread-parallel Minitest with exact totals`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
