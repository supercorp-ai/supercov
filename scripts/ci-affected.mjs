#!/usr/bin/env node
// Which of a pull request's CI jobs its change can affect, from Supercov's own
// answer: `supercov runs <map> tests affected --json` over the Self Coverage
// run of a commit on main the pull request descends from (the map). A job
// whose tests the change cannot reach is skipped before it takes a runner; a
// job of the workspace's tests runs only the affected ones.
//
//   node scripts/ci-affected.mjs triage [workflow]
//     How the run goes. A push to main, a run by hand, a release pull request
//     and one labelled `full-ci` run every job on every platform. Any other
//     pull request runs on Linux alone: in full where CI itself changed, as a
//     corpus check where only tests/compat changed, and otherwise from the
//     map. The merge queue's commit (a `merge_group` event) runs as the pull
//     request it was made for would, on what the group changes on main.
//     Prints JSON {platforms, mode, reason}.
//   node scripts/ci-affected.mjs plan <workflow> [answer directory]
//     Each job of the workflow (ci, compatibility, self-coverage, next, react)
//     with whether it runs, its matrix and why, as the `jobs` output; the
//     tests a partial job runs in selection.json; the reasons in the step
//     summary. The directory holds the answer: affected.json (`--json`),
//     names.txt (`--names`) and files.txt (`--files`). Without it, or with an
//     answer that cannot be trusted, every job runs in full.
//   node scripts/ci-affected.mjs nextest <selection.json> <nextest-list.json> [<part> <parts> [--all]]
//     The nextest filter for a partial run: the affected tests and every test
//     in a file the pull request changed (new ones), as a profile appended to
//     .config/nextest.toml (or to a copy of it named by NEXTEST_CONFIG_OUT).
//     With a part, only that part's share of them, dealt by their recorded
//     seconds; with --all, a share of every test. Prints how many it picked.
//   node scripts/ci-affected.mjs cargo-test <selection.json> <slice> <slices>
//     A partial Self Coverage slice's `cargo test` filter: its share of the
//     affected tests, by their recorded seconds.
//   node scripts/ci-affected.mjs tests-in <file>...
//     The tests written in the files named, by their libtest names: what a
//     pull request's Self Coverage measures beside the affected tests.
//   node scripts/ci-affected.mjs changed-gaps <base> <gaps.jsonl>...
//     The gaps of a partial Self Coverage run on lines changed since <base>
//     (`supercov runs <id> gaps --format jsonl`); exits 1 when there is one.
//
// Context comes from the environment GitHub Actions sets, plus CHANGED (a
// file listing the pull request's changed paths, one per line), SINCE_MAP (a
// file listing every path changed since the map's commit, the pull request's
// among them), MAP_REASON (why there is no map, where there is none) and
// CI_TEST_SECONDS (the test
// seconds of main's newest Self Coverage run, which outrank the committed
// .github/test-seconds.tsv; .github/actions/affected fetches them).
import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { withQueuedPull } from './ci-tiers.mjs';

// The checkout CI runs in: the working directory, as every step runs there.
const repository = process.cwd();
const read = (path) => readFileSync(resolve(repository, path), 'utf8');

// What decides how CI itself runs: a change here is not one a test's record
// can see, so everything runs. Dependency manifests and lockfiles are left to
// Supercov, whose fingerprint reads what they declare (a release's version
// bump is not a dependency change); it says so as `runWide`.
const RUNS_EVERYTHING = [
  '.github/actions/**',
  '.github/fetch-verified.sh',
  'rust-toolchain.toml',
  '.cargo/**',
  '**/build.rs',
  '.config/**',
  'scripts/ci-affected.mjs',
  'scripts/ci-tiers.mjs',
];
// And the workflow files a workflow runs by, its own and its caller's: a
// change to one workflow changes how no other runs. Every pull request that
// touched any of the repository's two dozen workflow files, or the seconds
// the jobs deal their tests by, ran every job of both tiers in full: a fix to
// the mutation benchmark's workflow measured Self Coverage for an hour.
const SLOW_TIER = '.github/workflows/slow-tier.yml';
const RUNS_EVERYTHING_OF = {
  ci: ['.github/workflows/ci.yml', 'scripts/engine-integrations-group.mjs'],
  compatibility: ['.github/workflows/compatibility.yml', SLOW_TIER],
  'self-coverage': ['.github/workflows/self-coverage.yml', 'scripts/ci-time-test.sh', SLOW_TIER],
  next: ['.github/workflows/next-compatibility.yml', SLOW_TIER],
  react: ['.github/workflows/react-compatibility.yml', SLOW_TIER],
};
// A workflow not named here (a call without one) keeps the old answer: any
// workflow file runs everything.
const runsEverything = (workflow) => [...RUNS_EVERYTHING, ...(RUNS_EVERYTHING_OF[workflow] ?? ['.github/**'])];

/** `**` any depth, `*` within one path segment. */
function glob(pattern) {
  const source = pattern
    .split('**')
    .map((part) => part.split('*').map((text) => text.replace(/[.+?^${}()|[\]\\]/gu, '\\$&')).join('[^/]*'))
    .join('.*');
  return new RegExp(`^${source.replace(/\.\*\//gu, '(?:.*/)?')}$`, 'u');
}
const matches = (file, patterns) => patterns.some((pattern) => glob(pattern).test(file));

function event() {
  const path = process.env.GITHUB_EVENT_PATH;
  const payload = path && existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {};
  return withQueuedPull(process.env.GITHUB_EVENT_NAME, payload);
}

function listed(path) {
  if (!path || !existsSync(path)) return null;
  return readFileSync(path, 'utf8').split('\n').map((line) => line.trim()).filter(Boolean);
}

function changedFiles() {
  return listed(process.env.CHANGED);
}

/**
 * The files whose tests the map may not know: the pull request's, and every
 * file changed since the map's commit. Main's map is measured on a schedule,
 * not on every merge, so it can be a few merges old: `tests affected` reads
 * the commits after it as changes, but a test written in one of them is in
 * no map, and only its file names it. Each of these files' tests runs.
 */
export function unmapped(changed, sinceMap = listed(process.env.SINCE_MAP) ?? []) {
  return [...new Set([...changed, ...sinceMap])];
}

// A pull request labelled for it runs every job on every platform, as a
// release pull request does.
export const FULL_LABEL = 'full-ci';
// What a corpus pull request changes: the compatibility corpus's records. Its
// check is the workspace's `compat` tests, on Linux.
const CORPUS = ['tests/compat/**'];

/**
 * How a run of `workflow` runs: on every platform (`all`) or on Linux alone,
 * and in full, from the map, as a corpus change, or not at all.
 */
export function triage(name = process.env.GITHUB_EVENT_NAME, payload = event(), changed = changedFiles(), workflow = '') {
  // The merge queue's commit runs as its pull request does, on the files the
  // group changes (.github/actions/affected lists them from the group's base).
  if (name !== 'pull_request' && name !== 'merge_group') {
    return { platforms: 'all', mode: 'full', reason: name === 'push' ? 'a push to main runs every test on every platform and keeps the map current' : `a ${name} run runs every test on every platform` };
  }
  const pull = payload.pull_request ?? {};
  // A label other than `full-ci` is a run of its own that runs nothing, and
  // the run of the push goes on: the slow tier's workflows run on a pull
  // request only when the slow tier calls them (scripts/ci-tiers.mjs), and
  // CI, the fast tier, ran on the push already.
  if (payload.action === 'labeled' && payload.label?.name !== FULL_LABEL) {
    return { platforms: 'linux', mode: 'skip', reason: `the label ${payload.label?.name} asks for nothing here` };
  }
  if (/^Release \d+\.\d+\.\d+/u.test(pull.title ?? '') || /^release\//u.test(pull.head?.ref ?? '')) {
    return { platforms: 'all', mode: 'full', reason: 'a release pull request runs every test on every platform' };
  }
  if ((pull.labels ?? []).some((label) => label.name === FULL_LABEL)) {
    return { platforms: 'all', mode: 'full', reason: `the \`${FULL_LABEL}\` label runs every test on every platform` };
  }
  if (changed === null) return { platforms: 'linux', mode: 'full', reason: 'the changed files could not be listed' };
  if (changed.length === 0) return { platforms: 'linux', mode: 'skip', reason: 'the pull request changes no file' };
  if (changed.every((file) => matches(file, CORPUS))) {
    return { platforms: 'linux', mode: 'corpus', reason: 'a change to the compatibility corpus alone: its records are checked by the `compat` tests' };
  }
  const everything = changed.filter((file) => matches(file, runsEverything(workflow)));
  if (everything.length > 0) {
    return { platforms: 'linux', mode: 'full', reason: `${everything.slice(0, 3).join(', ')}${everything.length > 3 ? ` and ${everything.length - 3} more` : ''} decide how CI runs` };
  }
  return { platforms: 'linux', mode: 'map', reason: '' };
}

// ------------------------------------------------------------------ the map

const GATES = 'crates/supercov-cli/tests/e2e_gates.rs';

/** The gates `e2e_gates.rs` declares now, by name, with their scripts. */
export function gatesNow(source = read(GATES)) {
  const gates = new Map();
  for (const [, name, script, args] of source.matchAll(/^ {4}([a-z0-9_]+) => "([^"]+)"(?: \[([^\]]*)\])?/gmu)) {
    const words = (args ?? '').split(',').map((word) => word.trim().replace(/^"|"$/gu, '')).filter(Boolean);
    // Before the gates named their scripts by path, by file name in scripts/.
    gates.set(name, [script.includes('/') ? script : `scripts/${script}`, ...words].join(' '));
  }
  return gates;
}

/**
 * Rust source with its comments and the insides of its string and character
 * literals blanked, lines kept: what is left is declarations and braces.
 */
function blankRust(source) {
  let out = '';
  let index = 0;
  const blank = (text) => text.replace(/[^\n]/gu, ' ');
  while (index < source.length) {
    const rest = source.slice(index);
    const raw = /^b?r(#*)"/u.exec(rest);
    let match;
    if (rest.startsWith('//')) {
      match = /^\/\/[^\n]*/u.exec(rest)[0];
    } else if (rest.startsWith('/*')) {
      let depth = 0;
      let end = 0;
      do {
        if (rest.startsWith('/*', end)) {
          depth += 1;
          end += 2;
        } else if (rest.startsWith('*/', end)) {
          depth -= 1;
          end += 2;
        } else {
          end += 1;
        }
      } while (depth > 0 && end < rest.length);
      match = rest.slice(0, end);
    } else if (raw) {
      const close = rest.indexOf(`"${raw[1]}`, raw[0].length);
      match = rest.slice(0, close < 0 ? rest.length : close + 1 + raw[1].length);
    } else if (/^b?"/u.test(rest)) {
      match = /^b?"(?:\\[\s\S]|[^"\\])*"?/u.exec(rest)[0];
    } else if (/^b?'(?:\\[\s\S][^']*|[^'\\])'/u.test(rest)) {
      match = /^b?'(?:\\[\s\S][^']*|[^'\\])'/u.exec(rest)[0];
    }
    if (match) {
      out += blank(match);
      index += match.length;
    } else {
      out += source[index];
      index += 1;
    }
  }
  return out;
}

/**
 * The module path of a Rust file in its test binary: `run::tests` for
 * `src/run/tests.rs`, empty for a crate root or an integration test's own
 * file, `common` for `tests/common/mod.rs`. `null` for a file no test binary
 * holds tests of (a build script, a benchmark, an example).
 */
function rustModule(file) {
  const parts = file.replace(/\.rs$/u, '').split('/');
  const at = (name) => parts.lastIndexOf(name);
  let inside;
  if (at('src') >= 0) {
    inside = parts.slice(at('src') + 1);
    // `src/bin/x.rs` and `src/bin/x/main.rs` are a binary's root.
    if (inside[0] === 'bin') inside = inside.length === 2 ? [] : inside.slice(2);
  } else if (at('tests') >= 0) {
    // `tests/cli.rs` is a test binary's root; `tests/common/mod.rs` is the
    // `common` module test binaries declare.
    inside = parts.slice(at('tests') + 1);
    if (inside.length === 1) inside = [];
  } else {
    return null;
  }
  if (['lib', 'main', 'mod'].includes(inside.at(-1))) inside = inside.slice(0, -1);
  return inside.join('::');
}

/** Every `#[test]` function a Rust file declares, by its libtest name. */
export function rustTests(file, source) {
  const module = rustModule(file);
  if (module === null) return [];
  const tests = [];
  const stack = [];
  let depth = 0;
  let test = false;
  for (const [token, attribute, modName, fnName] of blankRust(source).matchAll(/#!?\s*\[([^\]]*)\]|\bmod\s+([A-Za-z_]\w*)\s*\{|\bfn\s+([A-Za-z_]\w*)|[{}]/gu)) {
    if (attribute !== undefined) {
      // `#[test]`, `#[tokio::test]`: a path that ends in `test`.
      test ||= /^\s*(?:[A-Za-z_]\w*\s*::\s*)*test\s*(?:\(.*)?$/u.test(attribute);
    } else if (modName !== undefined) {
      depth += 1;
      stack.push({ name: modName, depth });
    } else if (fnName !== undefined) {
      if (test) tests.push([module, ...stack.map((entry) => entry.name), fnName].filter(Boolean).join('::'));
      test = false;
    } else if (token === '{') {
      depth += 1;
    } else {
      if (stack.at(-1)?.depth === depth) stack.pop();
      depth -= 1;
    }
  }
  return tests;
}

/**
 * The tests written in the files a pull request changed, which the map may
 * not know: every `#[test]` of a changed Rust file, and every gate when the
 * gates' file changed. A test the change added is among them.
 */
export function testsInFiles(changed, readFile = read) {
  const names = new Set();
  for (const file of changed.filter((path) => path.endsWith('.rs'))) {
    let source;
    try {
      source = readFile(file);
    } catch {
      continue; // removed by the change
    }
    for (const name of rustTests(file, source)) names.add(name);
    if (file === GATES) for (const gate of gatesNow(source).keys()) names.add(gate);
  }
  return [...names];
}

/**
 * Seconds each test or gate took in Self Coverage, where recorded: the larger
 * of its seconds and its processor seconds. Those of main's newest run
 * (CI_TEST_SECONDS, where the plan found them) outrank the committed ones,
 * which go stale as tests are added and grow.
 */
export function seconds(fresh = process.env.CI_TEST_SECONDS) {
  const table = new Map();
  const add = (text, replace) => {
    for (const line of text.split('\n')) {
      if (!line || line.startsWith('#')) continue;
      const [name, wall, cpu] = line.split('\t');
      const cost = Math.max(Number(wall) || 0, Number(cpu) || 0);
      table.set(name, replace ? cost : Math.max(cost, table.get(name) ?? 0));
    }
  };
  for (const path of ['.github/test-seconds.tsv', '.github/gate-seconds.tsv']) add(read(path), false);
  if (fresh && existsSync(fresh)) add(readFileSync(fresh, 'utf8'), true);
  return table;
}

/** A test's nextest seconds: about half its Self Coverage seconds. */
const nextestSeconds = (timing, name) => (timing.has(name) ? timing.get(name) / 2 : UNTIMED_SECONDS);

/** How many parts of CI's check job hold these tests at a few minutes each. */
export function partsFor(names, timing = seconds()) {
  const total = [...new Set(names)].reduce((sum, name) => sum + nextestSeconds(timing, name), 0);
  return Math.max(1, Math.min(MAX_PARTS, Math.ceil(total / PART_SECONDS)));
}

/**
 * Items dealt into `parts` piles of about equal cost: dearest first, each to
 * the lightest pile (ties by key, so every job deals the same). The first
 * pile may start with a head start of cost. Returns the piles' items.
 */
export function deal(items, parts, headStart = 0) {
  const loads = Array.from({ length: parts }, (_, index) => (index === 0 && parts > 1 ? headStart : 0));
  const piles = Array.from({ length: parts }, () => []);
  const sorted = [...items].sort((a, b) => b.cost - a.cost || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  for (const item of sorted) {
    const lightest = loads.indexOf(Math.min(...loads));
    loads[lightest] += item.cost;
    piles[lightest].push(item);
  }
  return piles;
}

/**
 * What the answer says, or why it cannot be used. `json` is `tests affected
 * --json` (whether to trust it); `names` and `files` are `--names` and
 * `--files`, every affected and undetermined test and test file.
 */
export function readAnswer({ json, names, files } = {}) {
  if (!json) return { untrusted: process.env.MAP_REASON || 'no map of a commit this pull request descends from' };
  if (json.ok === false) return { untrusted: `the map could not answer: ${json.error?.message ?? 'no reason given'}` };
  const data = json.data ?? json;
  const runWide = data.workingTree?.runWide ?? [];
  if (runWide.length > 0) return { untrusted: `${runWide.join('; ')} since the map` };
  if (data.workingTree?.sourceSetChanged) return { untrusted: 'a source or test file the map never saw' };
  if ((data.unmeasured ?? []).length > 0) return { untrusted: `changed code the map's Linux build never measured: ${data.unmeasured.slice(0, 3).join('; ')}` };
  if (!names || !files) return { untrusted: 'the map did not list the affected tests' };
  if (!(data.summary?.tests > 0)) return { untrusted: 'the map holds no test' };
  const reasons = new Map();
  for (const bucket of ['affected', 'undetermined']) {
    for (const test of data[bucket] ?? []) {
      const name = test.name.startsWith(`${test.file}::`) ? test.name.slice(test.file.length + 2) : test.name;
      reasons.set(name, test.reasons ?? []);
    }
  }
  return { run: new Set(names), files: new Set(files), reasons, changedFiles: (data.changedFiles ?? []).map((entry) => entry.file) };
}

// --------------------------------------------------------------- the jobs

// What each job runs, as the map names it: gates of e2e_gates.rs, the tests of
// some test files, or the workspace's tests nextest runs (every test that is
// not ignored). `paths` are inputs of steps Self Coverage does not measure
// (a launcher, a benchmark, a Go test of the launcher): a change to one runs
// the job. Each is a judgement, written down here, of what those steps read.
const SCRIPTS_OF_ENGINE_INTEGRATIONS = () =>
  JSON.parse(read('package.json'))
    .scripts['test:engine-integrations'].split(' && ')
    .filter((command) => command.startsWith('node scripts/'))
    .map((command) => command.slice('node '.length));

const BROWSER_WORKER = 'a_browser_worker_bootstraps_when_playwright_is_launched_by_a_unit_test';
const TOOLING = [
  'scripts/**', 'tests/release/**', 'tests/runtime/**', 'tests/action/**', 'action/**', 'action.yml',
  'runtime/**', 'crates/*/assets/**', 'crates/*/runtime-assets/**', 'crates/*/test-assets/**', 'docs/**',
  'plugins/**', 'bin/**', 'npm/**', 'packaging/**', 'cmd/**', '*.md', 'package.json', 'package-lock.json',
];
const LAUNCHERS = ['bin/**', 'npm/**', 'packaging/**', 'cmd/**', 'go.mod', 'go.sum', 'pyproject.toml', 'package.json',
  'scripts/launch-arguments-integration.mjs', 'scripts/package-preflight.mjs', 'crates/supercov-cli/src/main.rs'];

// The platforms a job's legs run on, where it has more than Linux: a pull
// request runs the Linux leg alone (GitHub runs five macOS jobs at a time for
// the whole organisation, and a push queued nineteen), and every leg runs on
// a release pull request, a push to main, a run by hand and `full-ci`.
const EVERY_OS = ['ubuntu-latest', 'ubuntu-24.04-arm', 'macos-latest', 'windows-latest', 'windows-11-arm'];
const LINUX = 'ubuntu-latest';

const WORKFLOWS = {
  ci: {
    // A pull request's affected tests run on one Node (24), in as many parts
    // as keep each to a few minutes, dealt by their recorded seconds; every
    // Node runs them all on a release, main and `full-ci`, in three parts.
    check: {
      nextest: true,
      shards: true,
      paths: TOOLING,
      matrix: (partial, parts = 1) => ({ node: partial ? [24] : [22, 24, 25], part: Array.from({ length: partial ? parts : FULL_PARTS }, (_, index) => index + 1) }),
    },
    'bun-bootstrap': { os: ['ubuntu-latest', 'macos-latest'], tests: (name, files) => files.has('crates/supercov-cli/tests/cli.rs') && name.startsWith('bun_') },
    'launch-arguments': { os: ['ubuntu-latest', 'macos-latest', 'windows-latest'], gates: ['rust_direct_node', 'node_test', 'rust_direct_vitest', 'rust_direct_jest'], paths: LAUNCHERS },
    lint: { paths: ['**/*.rs', 'Cargo.toml', 'Cargo.lock', '**/Cargo.toml', 'clippy.toml', 'rustfmt.toml', '.rustfmt.toml'] },
    'engine-integrations': { engineIntegrations: true },
    // The release gates (a release build, the packed install, the benchmark
    // against the last release) are the slow tier's: release-gates.yml.
    // Apple clang's own MC/DC is its oracle, so it runs on macOS alone: on a
    // pull request, only when its own files change.
    'clang-mcdc-oracle': { os: ['macos-latest'], ownPlatform: true, paths: ['scripts/clang-mcdc-oracle.mjs', 'tests/fixtures/clang-mcdc/**'] },
    'kotlin-grammar': {
      paths: ['crates/supercov-engine/vendor/tree-sitter-kotlin/**', 'crates/supercov-engine/src/kotlin_grammar.rs',
        'crates/supercov-engine/tests/kotlin_corpus.rs', 'scripts/kotlin-*'],
    },
    'rust-worker': { gateLegs: ['rust_worker_playwright', 'rust_worker_cargo_test', 'rust_worker_durable_objects', 'rust_worker_mutants'] },
    'rust-wasm-js': { gates: ['rust_wasm_js'] },
    'rust-wasm': { gates: ['rust_wasm_hosts'] },
  },
  compatibility: {
    node: { gates: ['rust_fixture_matrix', 'engine_contract'], paths: ['bin/**', 'tests/fixtures/no-build-node/**'] },
    runners: { gates: ['rust_fixture_matrix'] },
    browsers: { gates: ['rust_fixture_matrix', 'isolation', 'opaque_runner', 'opaque_esm'], paths: ['bin/**', 'tests/fixtures/generic-playwright/**'] },
    filesystems: {
      os: ['ubuntu-24.04-arm', 'macos-latest', 'windows-latest', 'windows-11-arm'],
      nextest: true,
      gates: ['rust_process_supervision', 'workspace_crash'],
      matrix: (partial) => ({ partition: partial ? [1] : [1, 2, 3] }),
    },
    python: {
      os: EVERY_OS,
      gates: ['python', 'failure_output_python', 'side_by_side_python'],
      paths: ['runtime/python/**', 'crates/supercov-engine/runtime-assets/python/**', 'tests/runtime/python-*',
        'scripts/python-benchmark.mjs', 'scripts/python-startup-benchmark.py', 'crates/supercov-engine/src/python_*.rs'],
    },
    ruby: { os: EVERY_OS, gates: ['ruby_coverage', 'failure_output_ruby', 'side_by_side_ruby'] },
    rust: { os: EVERY_OS, gates: ['rust_public_cargo', 'failure_output_rust', 'side_by_side_rust'] },
    go: {
      os: EVERY_OS,
      gates: ['failure_output_go', 'side_by_side_go'],
      testFiles: ['crates/supercov-engine/tests/go_frontend.rs', 'crates/supercov-engine/tests/go_run.rs'],
      paths: ['cmd/**', 'go.mod', 'go.sum'],
    },
    jvm: {
      os: EVERY_OS,
      gates: ['side_by_side_jvm'],
      testFiles: ['crates/supercov-engine/tests/jvm_frontend.rs', 'crates/supercov-engine/tests/jvm_run.rs'],
    },
  },
  next: { application: { gates: ['next_playwright'], paths: ['tests/fixtures/next-playwright/**', 'scripts/jev-stand-in.mjs'] } },
  react: { focused: { gates: ['react_hydration', 'rust_vitest_projects'], paths: ['examples/react-verification/**'] } },
  'self-coverage': { measure: { selfCoverage: true } },
};

const SLICE_SECONDS = 600;
const MAX_SLICES = 10;
// CI's check job: a part's share of the tests, in nextest seconds, which are
// about half of Self Coverage's (no instrumentation): some three minutes on
// the runner's four processors. Every Node runs every test in three parts.
const PART_SECONDS = 700;
const MAX_PARTS = 6;
const FULL_PARTS = 3;
// A test no run has timed, in nextest seconds.
const UNTIMED_SECONDS = 3;
// The first part also runs the doctests and the tooling checks, about two
// and a half minutes on four processors: it starts with that much.
const FIRST_PART_HEAD_START = 500;
// Engine integrations: a group's share of the affected scripts, in seconds.
const GROUP_SECONDS = 420;
const MAX_GROUPS = 6;

/** Each job of `workflow`: whether it runs, its matrix, why, and the tests of a partial one. */
export function plan(workflow, answer, mode = { platforms: 'linux', mode: 'map', reason: '' }, changed = changedFiles() ?? []) {
  const specs = WORKFLOWS[workflow];
  if (!specs) throw new Error(`unknown workflow ${workflow}`);
  const gates = gatesNow();
  const read = mode.mode === 'map' ? readAnswer(answer) : {};
  const everything = mode.mode === 'full' || Boolean(read.untrusted);
  const why = mode.mode === 'full' ? mode.reason : read.untrusted;
  const jobs = {};
  const run = read.run ?? new Set();
  const files = read.files ?? new Set();
  const gatesRun = files.has(GATES) ? [...gates.keys()].filter((gate) => run.has(gate)) : [];
  const unit = [...run].filter((name) => !gatesRun.includes(name));
  // Tests are picked by the file they are written in from these: a test the
  // map cannot name (new in the pull request, or on main since the map) is
  // in one of them.
  const fresh = unmapped(changed);
  const selection = { names: [...run], changed: fresh, reasons: Object.fromEntries(read.reasons ?? []), binaries: [] };
  const ranGates = (names) => names.filter((name) => gatesRun.includes(name));
  const touched = (patterns) => changed.filter((file) => matches(file, patterns ?? []));
  const timing = seconds();
  for (const [name, spec] of Object.entries(specs)) {
    const own = spec.ownPlatform && mode.mode !== 'skip' && touched(spec.paths).length > 0;
    const os = spec.os && (mode.platforms === 'all' || own ? spec.os : spec.os.filter((each) => each === LINUX));
    if (mode.mode === 'skip') {
      jobs[name] = { run: false, mode: 'skip', why: mode.reason, matrix: spec.matrix?.(true), os };
      continue;
    }
    if (os?.length === 0) {
      jobs[name] = { run: false, mode: 'skip', why: `it runs on ${spec.os.join(', ')} alone: on release pull requests, main, a run by hand and \`${FULL_LABEL}\``, matrix: spec.matrix?.(true), os: spec.os };
      continue;
    }
    if (mode.mode === 'corpus') {
      const check = name === 'check';
      if (check) selection.binaries = ['compat'];
      jobs[name] = check
        ? { run: true, mode: 'partial', why: mode.reason, matrix: spec.matrix(true), parts: 1 }
        : { run: false, mode: 'skip', why: mode.reason, matrix: spec.matrix?.(true), os };
      continue;
    }
    if (everything) {
      jobs[name] = { run: true, mode: 'full', why: `${why}${mode.platforms === 'linux' && spec.os ? ' (Linux: a pull request runs no other platform)' : ''}`, matrix: fullMatrix(spec), os, ...(spec.shards ? { parts: FULL_PARTS } : {}) };
      continue;
    }
    const reasons = [];
    const paths = touched(spec.paths);
    if (paths.length > 0) reasons.push(`${paths.slice(0, 2).join(', ')}${paths.length > 2 ? ` and ${paths.length - 2} more` : ''} changed`);
    const gatesHit = ranGates(spec.gates ?? []);
    if (gatesHit.length > 0) reasons.push(`gates ${gatesHit.join(', ')} affected`);
    const testsHit = spec.tests ? unit.filter((test) => spec.tests(test, files)) : [];
    if (testsHit.length > 0) reasons.push(`${testsHit.length} of its tests affected`);
    const filesHit = (spec.testFiles ?? []).filter((file) => files.has(file));
    if (filesHit.length > 0) reasons.push(`tests of ${filesHit.join(', ')} affected`);
    if (spec.nextest) {
      // Its tests run in part: the affected ones, and the ones the map does
      // not know, found in the job's own listing.
      const ownTests = [...files].some((file) => file !== GATES);
      const rustChanged = fresh.some((file) => file.endsWith('.rs'));
      if (ownTests) reasons.push(`${unit.length} workspace test(s) affected`);
      else if (rustChanged) reasons.push('Rust changed: tests new since the map run');
      // As many parts as hold the affected tests and the ones written in
      // the changed files at a few minutes each.
      const parts = spec.shards ? partsFor([...unit, ...testsInFiles(fresh)], timing) : 1;
      jobs[name] = reasons.length === 0
        ? { run: false, mode: 'skip', why: 'none of its tests is affected', matrix: spec.matrix(true) }
        : { run: true, mode: 'partial', why: `${reasons.join('; ')}${parts > 1 ? `; in ${parts} parts` : ''}`, matrix: spec.matrix(true, parts), parts };
      continue;
    }
    if (spec.engineIntegrations) {
      const commands = SCRIPTS_OF_ENGINE_INTEGRATIONS();
      const byCommand = new Map([...gates].map(([gate, command]) => [command, gate]));
      // A script no gate runs is not in the map: it runs.
      const picked = commands.filter((command) => !byCommand.has(command) || gatesRun.includes(byCommand.get(command)));
      const browser = files.has('crates/supercov-cli/tests/cli.rs') && run.has(BROWSER_WORKER);
      if (picked.length === 0 && !browser) {
        jobs[name] = { run: false, mode: 'skip', why: 'none of its scripts is affected' };
        continue;
      }
      const total = picked.reduce((sum, command) => sum + (timing.get(byCommand.get(command)) ?? 60), 0);
      const groups = Math.max(1, Math.min(MAX_GROUPS, Math.ceil(total / GROUP_SECONDS)));
      selection.scripts = picked;
      jobs[name] = {
        run: true,
        mode: 'partial',
        why: `${picked.length} of ${commands.length} scripts affected${browser ? '; the browser worker test affected' : ''}`,
        matrix: { include: Array.from({ length: groups }, (_, index) => ({ group: index + 1, groups })) },
        scripts: picked.join(','),
        browser,
      };
      continue;
    }
    if (spec.gateLegs) {
      const legs = ranGates(spec.gateLegs).map((gate) => gate.replaceAll('_', '-'));
      jobs[name] = legs.length === 0
        ? { run: false, mode: 'skip', why: 'none of its gates is affected' }
        : { run: true, mode: 'partial', why: `gates ${legs.join(', ')} affected`, matrix: { gate: legs } };
      continue;
    }
    if (spec.selfCoverage) {
      // Every test to run: the affected ones, and every test written in a
      // file the change touched -- one it added is in no map, and the lines
      // it was written for are held to the floor. Dealt into as few slices
      // as hold them at about ten minutes each.
      const written = testsInFiles(fresh);
      const measured = new Set([...run, ...written]);
      selection.names = [...measured];
      const total = [...measured].reduce((sum, test) => sum + (timing.get(test) ?? 1), 0);
      const slices = Math.max(1, Math.min(MAX_SLICES, Math.ceil(total / SLICE_SECONDS)));
      jobs[name] = measured.size === 0
        ? { run: false, mode: 'skip', why: 'no test is affected or written in a changed file' }
        : { run: true, mode: 'partial', why: `${run.size} affected test(s) and ${written.length} in changed files, about ${Math.round(total / 60)} minutes`, matrix: { slice: Array.from({ length: slices }, (_, index) => index) }, slices };
      continue;
    }
    jobs[name] = reasons.length === 0
      ? { run: false, mode: 'skip', why: 'none of its tests is affected' }
      : { run: true, mode: 'full', why: reasons.join('; ') };
  }
  for (const [name, spec] of Object.entries(specs)) {
    const own = spec.ownPlatform && touched(spec.paths).length > 0;
    if (spec.os && jobs[name].os === undefined) jobs[name].os = mode.platforms === 'all' || own ? spec.os : spec.os.filter((each) => each === LINUX);
  }
  return { jobs, selection, why };
}

function fullMatrix(spec) {
  if (spec.matrix) return spec.matrix(false);
  if (spec.engineIntegrations) return { include: [1, 2, 3, 4].map((group) => ({ group, groups: 4 })) };
  if (spec.gateLegs) return { gate: spec.gateLegs.map((gate) => gate.replaceAll('_', '-')) };
  if (spec.selfCoverage) return { slice: Array.from({ length: MAX_SLICES }, (_, index) => index) };
  return undefined;
}

function summary(workflow, result, mode) {
  const lines = [`### Affected tests: ${workflow}`, ''];
  lines.push(mode.platforms === 'all' ? 'Every platform.' : `Linux alone: macOS, Windows and ARM legs run on release pull requests, main, a run by hand and the \`${FULL_LABEL}\` label.`, '');
  if (mode.mode !== 'map' || result.why) lines.push(`${mode.mode === 'skip' ? 'Every job is skipped' : mode.mode === 'corpus' ? 'A corpus change' : 'Every job runs in full'}: ${mode.mode === 'map' ? result.why : mode.reason}.`, '');
  lines.push('| Job | Runs | Why |', '| --- | --- | --- |');
  for (const [name, job] of Object.entries(result.jobs)) {
    const runs = !job.run ? 'skipped' : job.mode === 'full' ? 'in full' : job.whole ? 'its tests, in part' : 'in part';
    lines.push(`| ${name} | ${runs} | ${(job.why ?? '').replaceAll('|', '\\|')} |`);
  }
  const names = result.selection.names;
  if (names.length > 0) {
    lines.push('', `<details><summary>${names.length} test(s) affected</summary>`, '');
    for (const name of names.slice(0, 200)) lines.push(`- \`${name}\`${result.selection.reasons[name]?.[0] ? `: ${result.selection.reasons[name][0].replaceAll('|', '\\|')}` : ''}`);
    if (names.length > 200) lines.push(`- and ${names.length - 200} more`);
    lines.push('', '</details>');
  }
  return `${lines.join('\n')}\n`;
}

// ------------------------------------------------------------- the filters

/**
 * The workspace files a test of a nextest binary may be written in: an
 * integration test's file, or for a library or binary's own test, the module
 * files its path passes through (`run::tests::it` is in `src/run.rs`,
 * `src/run/mod.rs` or `src/run/tests.rs`, or the crate root).
 */
function sourceFiles(suite, name, root) {
  const directory = suite.cwd.startsWith(root) ? suite.cwd.slice(root.length).replace(/^[\\/]/u, '').replaceAll('\\', '/') : suite.cwd;
  if (suite.kind === 'test') return [`${directory}/tests/${suite['binary-name']}.rs`];
  const files = [`${directory}/src/${suite.kind === 'bin' ? 'main' : 'lib'}.rs`];
  const segments = name.split('::').slice(0, -1);
  for (let index = 1; index <= segments.length; index += 1) {
    const path = segments.slice(0, index).join('/');
    files.push(`${directory}/src/${path}.rs`, `${directory}/src/${path}/mod.rs`);
  }
  return files;
}

/**
 * A nextest filter for a partial run: the affected tests, and every test in
 * a file the pull request changed -- a test added since the map is in one,
 * and the map cannot name it. With `part` of `parts`, that part's share of
 * them, dealt by their recorded seconds so the parts take about as long;
 * with `all`, a share of every test.
 */
export function nextestFilter(selection, listing, root = repository, { part = 1, parts = 1, all = false, timing } = {}) {
  const wanted = new Set(selection.names);
  const changed = new Set(selection.changed);
  const binaries = new Set(selection.binaries ?? []);
  const chosen = [];
  const sizes = new Map();
  for (const [binary, suite] of Object.entries(listing['rust-suites'] ?? {})) {
    const cases = Object.entries(suite.testcases ?? {}).filter(([, test]) => !test.ignored);
    sizes.set(binary, cases.length);
    const whole = all || binaries.has(suite['binary-name']);
    for (const [name] of cases) {
      if (whole || wanted.has(name) || sourceFiles(suite, name, root).some((file) => changed.has(file))) chosen.push({ binary, name });
    }
  }
  let mine = chosen;
  if (parts > 1) {
    const table = timing ?? seconds();
    const items = chosen.map((test) => ({ ...test, key: `${test.binary} ${test.name}`, cost: nextestSeconds(table, test.name) }));
    mine = deal(items, parts, FIRST_PART_HEAD_START)[part - 1];
  }
  const byBinary = new Map();
  for (const test of mine) byBinary.set(test.binary, [...(byBinary.get(test.binary) ?? []), test.name]);
  const terms = [];
  for (const [binary, names] of [...byBinary].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    terms.push(names.length === sizes.get(binary)
      ? `binary_id(${binary})`
      : `(binary_id(${binary}) & (${names.sort().map((name) => `test(=${name})`).join(' | ')}))`);
  }
  return { expression: terms.join(' | '), picked: mine.length, of: chosen.length };
}

/** The tests of one slice of a partial Self Coverage run, dealt by seconds. */
export function sliceTests(selection, slice, slices) {
  const timing = seconds();
  const tests = selection.names.map((name) => ({ name, cost: timing.get(name) ?? 1 }))
    .sort((a, b) => b.cost - a.cost || a.name.localeCompare(b.name));
  const load = Array.from({ length: slices }, () => 0);
  const mine = [];
  for (const test of tests) {
    const lightest = load.indexOf(Math.min(...load));
    load[lightest] += test.cost;
    if (lightest === slice) mine.push(test.name);
  }
  return mine;
}

/** The lines each file gained or changed since `base`, from git's own diff. */
export function changedLines(base) {
  const diff = execFileSync('git', ['diff', '--unified=0', '--no-color', base, 'HEAD'], { encoding: 'utf8', maxBuffer: 1 << 30 });
  const lines = new Map();
  let file = null;
  for (const line of diff.split('\n')) {
    if (line.startsWith('+++ ')) file = line.startsWith('+++ b/') ? line.slice(6) : null;
    const hunk = /^@@ -\S+ \+(\d+)(?:,(\d+))? @@/u.exec(line);
    if (hunk && file) {
      const [start, count] = [Number(hunk[1]), hunk[2] === undefined ? 1 : Number(hunk[2])];
      const set = lines.get(file) ?? new Set();
      for (let n = start; n < start + count; n += 1) set.add(n);
      lines.set(file, set);
    }
  }
  return lines;
}

// ------------------------------------------------------------------- main

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith('ci-affected.mjs')) {
  const [command, ...rest] = process.argv.slice(2);
  if (command === 'triage') {
    console.log(JSON.stringify(triage(undefined, undefined, undefined, rest[0])));
  } else if (command === 'plan') {
    const [workflow, directory] = rest;
    const mode = triage(undefined, undefined, undefined, workflow);
    const answer = {};
    const file = (name) => (directory && existsSync(resolve(directory, name)) ? readFileSync(resolve(directory, name), 'utf8') : null);
    if (mode.mode === 'map' && file('affected.json')) {
      try {
        answer.json = JSON.parse(file('affected.json'));
      } catch (error) {
        answer.json = { ok: false, error: { message: `its answer could not be read: ${error.message}` } };
      }
      const lines = (name) => file(name)?.split('\n').map((line) => line.trim()).filter(Boolean) ?? null;
      answer.names = lines('names.txt');
      answer.files = lines('files.txt');
    }
    const result = plan(workflow, answer, mode);
    writeFileSync('selection.json', JSON.stringify(result.selection));
    if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `jobs=${JSON.stringify(result.jobs)}\n`);
    if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary(workflow, result, mode));
    console.log(JSON.stringify(result.jobs, null, 1));
  } else if (command === 'changed-gaps') {
    const [base, ...files] = rest;
    const lines = changedLines(base);
    const gaps = files.filter((file) => existsSync(file)).flatMap((file) =>
      readFileSync(file, 'utf8').split('\n').filter(Boolean).map((line) => JSON.parse(line)))
      .filter((gap) => lines.get(gap.file)?.has(gap.line));
    const report = [`### Changed lines`, '', gaps.length === 0
      ? `Every line this pull request adds or changes is covered (${[...lines.values()].reduce((n, set) => n + set.size, 0)} changed line(s) read).`
      : `${gaps.length} gap(s) on lines this pull request adds or changes:`, ''];
    for (const gap of gaps.slice(0, 100)) report.push(`- \`${gap.file}:${gap.line}\` ${gap.kind ?? ''} ${gap.detail ?? gap.message ?? ''}`.trimEnd());
    console.log(report.join('\n'));
    if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${report.join('\n')}\n`);
    process.exit(gaps.length === 0 ? 0 : 1);
  } else if (command === 'nextest') {
    const all = rest.includes('--all');
    const [selectionPath, listingPath, part = '1', parts = '1'] = rest.filter((word) => word !== '--all');
    const { expression, picked, of } = nextestFilter(JSON.parse(readFileSync(selectionPath, 'utf8')), JSON.parse(readFileSync(listingPath, 'utf8')), repository, { part: Number(part), parts: Number(parts), all });
    const config = resolve(repository, '.config/nextest.toml');
    const out = process.env.NEXTEST_CONFIG_OUT ? resolve(process.env.NEXTEST_CONFIG_OUT) : config;
    if (out !== config) writeFileSync(out, readFileSync(config, 'utf8'));
    appendFileSync(out, `\n[profile.affected]\ndefault-filter = '''${expression || 'none()'}'''\n`);
    console.log(`[ci-affected] ${picked} test(s) picked${Number(parts) > 1 ? ` for part ${part} of ${parts} (${of} in all)` : ''}`);
  } else if (command === 'tests-in') {
    // The tests a Self Coverage run of a pull request measures beside the
    // affected ones: those written in the files named.
    for (const name of testsInFiles(rest)) console.log(name);
  } else if (command === 'cargo-test') {
    const [selectionPath, slice, slices] = rest;
    const names = sliceTests(JSON.parse(readFileSync(selectionPath, 'utf8')), Number(slice), Number(slices));
    console.log(names.length > 0 ? names.join(' ') : 'no_test_was_dealt_to_this_slice');
  } else {
    console.error('usage: ci-affected.mjs triage | plan <workflow> [answer directory] | nextest <selection.json> <list.json> [<part> <parts> [--all]] | cargo-test <selection.json> <slice> <slices> | tests-in <file>... | changed-gaps <base> <gaps.jsonl>...');
    process.exit(2);
  }
}
