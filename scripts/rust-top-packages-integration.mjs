// What failed running the test suites of popular npm packages under Supercov,
// through the public command, each in the smallest project that shows it.
//
// - semver, js-yaml and picomatch lint before they test. ESLint read the
//   instrumented copies, and Supercov's own .supercov/*.mjs, and failed on
//   code nobody wrote. A linter or formatter now reads what the author wrote.
// - uuid compiles its TypeScript tests with tsc --strict. A wrapped assertion
//   ran in a callback, which TypeScript narrows nothing into (TS18048,
//   TS7034, TS7005), and an `asserts` signature narrowed nothing after it.
// - commander reads { stdout } from util.promisify(execFile), which resolved
//   the bare string under Supercov; chalk passes its fixtures an environment
//   of their own, which Supercov extended with the parent's CI.
// - ms runs its suite twice, and the second run's phases repeated the first's,
//   so the run could not be opened.
// - A project's own coverage tool measures the instrumented copy, whose probes
//   are branches no test was written to cover: c8, Jest and Vitest failed a
//   100% gate on a suite that covers every branch. They still report; their
//   thresholds are not checked, and a failing test still fails the run.
// - What those tools report is what they report without Supercov: Node's own
//   coverage, Jest and Vitest read the same figures on code with every probe
//   form, and Node's thresholds judge that same report.
import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-top-packages-'));

function write(root, path, text) {
  mkdirSync(resolve(root, path, '..'), { recursive: true });
  writeFileSync(resolve(root, path), text);
}

function supercov(cwd, args, env = {}) {
  const result = spawnSync(binary, args, {
    cwd,
    encoding: 'utf8',
    env: { ...process.env, CI: '1', NO_COLOR: '1', ...env },
    timeout: 240_000,
  });
  return { status: result.status, output: `${result.stdout}\n${result.stderr}` };
}

// The repository's own copies of real tools, linked the way npm installs them.
function link(root, packages, bins) {
  mkdirSync(resolve(root, 'node_modules/.bin'), { recursive: true });
  for (const name of packages) {
    mkdirSync(resolve(root, 'node_modules', name, '..'), { recursive: true });
    symlinkSync(resolve(repository, 'node_modules', name), resolve(root, 'node_modules', name));
  }
  for (const name of bins) {
    symlinkSync(resolve(repository, 'node_modules/.bin', name), resolve(root, 'node_modules/.bin', name));
  }
}

const skipped = (tool) => new RegExp(`\\[supercov\\] ${tool}'s coverage thresholds were not checked`, 'g');

function covered(cwd) {
  const summary = supercov(cwd, ['runs', 'latest']);
  assert.equal(summary.status, 0, summary.output);
  const lines = summary.output.match(/Lines\s+([\d.]+)% \((\d+)\/(\d+)\)/);
  assert.ok(lines, summary.output);
  return { covered: Number(lines[2]), total: Number(lines[3]), output: summary.output };
}

// Stand-ins for ESLint and Prettier: each walks the project the way the real
// tool does and fails on anything Supercov wrote. The preload recognises the
// tools by where their entry point lives.
const linter = (walk) => `
const fs = require('node:fs');
const path = require('node:path');
const seen = [];
const judge = (file, text) => {
  const local = path.relative(process.cwd(), file);
  seen.push(local);
  if (local.split(path.sep).includes('.supercov')) throw new Error('listed ' + local);
  if (/__supercov|__SUPERCOV|sourceMappingURL=data:/.test(text)) throw new Error('instrumented ' + file);
};
${walk}
`;

try {
  const lint = resolve(temporary, 'lint');
  write(lint, 'package.json', JSON.stringify({
    name: 'lint-fixture',
    private: true,
    scripts: {
      lint: 'node node_modules/eslint/bin/eslint.js && node node_modules/prettier/bin/prettier.cjs',
      test: 'npm run lint && node --test && node --test',
    },
  }));
  write(lint, 'lib/index.js', `'use strict';
exports.pick = function pick(value, fallback) {
  if (value && value.length > 0) return value;
  return fallback;
};
`);
  write(lint, 'lib/show.js', `process.stdout.write(JSON.stringify({ argv: process.argv.slice(2), ci: process.env.CI ?? null, own: process.env.OWN ?? null }));
`);
  write(lint, 'test/index.test.js', `'use strict';
const assert = require('node:assert/strict');
const { execFile, execFileSync } = require('node:child_process');
const { promisify } = require('node:util');
const test = require('node:test');
const { pick } = require('../lib/index.js');
const show = require('node:path').join(__dirname, '../lib/show.js');

test('picks', () => {
  assert.equal(pick('a', 'b'), 'a');
  assert.equal(pick('', 'b'), 'b');
});

test('a promisified execFile resolves stdout and stderr', async () => {
  const { stdout, stderr } = await promisify(execFile)(process.execPath, [show, 'x']);
  assert.equal(JSON.parse(stdout).argv[0], 'x');
  assert.equal(stderr, '');
});

test('a child given its own environment gets only that', () => {
  const seen = JSON.parse(execFileSync(process.execPath, [show], { env: { OWN: '1' }, encoding: 'utf8' }));
  assert.equal(seen.ci, null);
  assert.equal(seen.own, '1');
});
`);
  write(lint, 'node_modules/eslint/bin/eslint.js', linter(`
const walk = (directory) => {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const file = path.join(directory, entry.name);
    if (entry.name === 'node_modules') continue;
    if (entry.isDirectory()) walk(file);
    else if (/\\.m?[jt]s$/.test(entry.name)) judge(file, fs.readFileSync(file, 'utf8'));
  }
};
walk(process.cwd());
if (!seen.includes(path.join('lib', 'index.js')) || !seen.includes(path.join('test', 'index.test.js'))) throw new Error('did not read ' + seen);
`));
  write(lint, 'node_modules/prettier/bin/prettier.cjs', linter(`
(async () => {
  for (const entry of await fs.promises.readdir(process.cwd(), { recursive: true })) {
    if (entry.split(path.sep).includes('node_modules') || !/\\.m?[jt]s$/.test(entry)) continue;
    judge(path.resolve(entry), await fs.promises.readFile(entry, 'utf8'));
  }
  await new Promise((done, fail) => fs.readFile('lib/index.js', 'utf8', (error, text) => {
    if (error) return fail(error);
    try { judge(path.resolve('lib/index.js'), text); done(); } catch (problem) { fail(problem); }
  }));
})().catch((error) => { console.error(error.message); process.exit(1); });
`));
  const linted = supercov(lint, ['--', 'npm', 'test']);
  assert.equal(linted.status, 0, linted.output);
  const lintCoverage = covered(lint);
  assert.ok(lintCoverage.covered > 0, lintCoverage.output);

  const typed = resolve(temporary, 'typed');
  mkdirSync(resolve(typed, 'node_modules/.bin'), { recursive: true });
  symlinkSync(resolve(repository, 'node_modules/typescript'), resolve(typed, 'node_modules/typescript'));
  symlinkSync(resolve(repository, 'node_modules/.bin/tsc'), resolve(typed, 'node_modules/.bin/tsc'));
  mkdirSync(resolve(typed, 'node_modules/@types'), { recursive: true });
  symlinkSync(resolve(repository, 'node_modules/@types/node'), resolve(typed, 'node_modules/@types/node'));
  write(typed, 'package.json', JSON.stringify({
    name: 'typed-fixture',
    private: true,
    type: 'module',
    scripts: {
      build: 'tsc -p tsconfig.json',
      pretest: 'npm run build',
      test: 'node --test "dist/test/*.test.js"',
    },
  }));
  write(typed, 'tsconfig.json', JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'NodeNext',
      moduleResolution: 'NodeNext',
      rootDir: 'src',
      outDir: 'dist',
      strict: true,
      types: ['node'],
    },
    include: ['src/**/*.ts'],
  }));
  write(typed, 'src/ids.ts', `export function next(step: number): string {
  return String(1000 + step).padStart(8, '0');
}
export function find(name: string): { name: string } | undefined {
  return name ? { name } : undefined;
}
// Every probe form a strict compile has to accept: selections, optional
// members and calls, an enumeration loop and a caught exception.
export function describe(options: { label?: string | null; format?: (value: number) => string; values?: number[] }): string {
  const label = options.label ?? 'none';
  const format = options.format || String;
  let total = 0;
  for (const value of options.values ?? []) total += value;
  try {
    JSON.parse(label);
  } catch {
    total += 1;
  }
  const shown = options.format?.(total) ?? format(total);
  return options.values?.length && label ? label + ':' + shown : shown;
}
`);
  write(typed, 'src/test/ids.test.ts', `import assert from 'node:assert/strict';
import test from 'node:test';
import { describe, find, next } from '../ids.js';

test('ids sort by creation', () => {
  let prior: string | undefined;
  for (let i = 0; i < 5; i++) {
    const id = next(i);
    if (prior !== undefined) {
      assert.ok(prior < id, \`\${prior} < \${id}\`);
    }
    prior = id;
  }
  const ids = [];
  for (let i = 0; i < 3; i++) ids.push(next(i));
  assert.deepEqual(ids, ids.slice().sort())
});

test('every probe form survives a strict compile', () => {
  assert.equal(describe({ label: 'a', values: [1, 2] }), 'a:4');
  assert.equal(describe({ format: (value) => '#' + value }), '#1');
});

test('an asserts signature narrows what follows', () => {
  const user = find('ada');
  assert.ok(user);
  if (user.name) assert.equal(user.name.length, 3);
  const length: number = ((): number => user.name.length)();
  assert.equal(length, 3);
  // @ts-expect-error the directive stays on the line it guards
  assert.equal(user.missing, undefined);
  assert.throws(() => {
    assert.equal(user.name, 'bob');
  });
});
`);
  const built = supercov(typed, ['--', 'npm', 'test']);
  assert.equal(built.status, 0, built.output.slice(-6000));
  assert.doesNotMatch(built.output, /error TS\d+/, built.output);
  const typedCoverage = covered(typed);
  assert.ok(typedCoverage.covered > 0, typedCoverage.output);

  // c8 and nyc as stand-ins with the layout the preload patches: c8's report
  // takes checkCoverages from check-coverage.js when it loads and its
  // check-coverage command calls the export; nyc checks through
  // NYC.prototype.checkCoverage. Each check fails the way the real tools
  // judged Supercov's probes.
  const gates = resolve(temporary, 'gates');
  write(gates, 'package.json', JSON.stringify({
    name: 'gates-fixture',
    private: true,
    scripts: {
      test: 'c8 --check-coverage --100 node --test && nyc --check-coverage node --test && c8 check-coverage && nyc check-coverage && karma start',
    },
  }));
  write(gates, 'lib/index.js', `'use strict';
exports.pick = function pick(value, fallback) {
  if (value && value.length > 0) return value;
  return fallback;
};
`);
  write(gates, 'test/index.test.js', `'use strict';
const assert = require('node:assert/strict');
const test = require('node:test');
const { pick } = require('../lib/index.js');

test('picks', () => {
  assert.equal(pick('a', 'b'), 'a');
  assert.equal(pick('', 'b'), 'b');
});

test('fails when asked to', () => {
  assert.equal(process.env.FAIL_ONE, undefined);
});
`);
  write(gates, 'node_modules/c8/lib/commands/check-coverage.js', `exports.handler = (argv) => { exports.checkCoverages(argv); };
exports.checkCoverages = async () => {
  process.exitCode = 1;
  console.error('ERROR: Coverage for branches (80%) does not meet global threshold (100%)');
};
`);
  write(gates, 'node_modules/c8/lib/commands/report.js', `const { checkCoverages } = require('./check-coverage');
exports.outputReport = async (argv) => {
  console.log('c8 report');
  if (argv.checkCoverage) await checkCoverages(argv);
};
`);
  write(gates, 'node_modules/c8/bin/c8.js', `#!/usr/bin/env node
const { spawnSync } = require('node:child_process');
const { outputReport } = require('../lib/commands/report');
const args = process.argv.slice(2);
if (args[0] === 'check-coverage') {
  require('../lib/commands/check-coverage').handler({});
} else {
  const at = args.findIndex((arg) => !arg.startsWith('-'));
  const child = spawnSync(args[at], args.slice(at + 1), { stdio: 'inherit' });
  outputReport({ checkCoverage: args.includes('--check-coverage') || args.includes('--100') })
    .then(() => process.exit(process.exitCode || child.status));
}
`);
  write(gates, 'node_modules/nyc/index.js', `module.exports = class NYC {
  constructor() {
    this.config = { excludeAfterRemap: true };
  }
  async getCoverageMapFromAllCoverageFiles() {
    console.log('nyc excludeAfterRemap=' + this.config.excludeAfterRemap);
  }
  async checkCoverage() {
    process.exitCode = 1;
    console.error('ERROR: Coverage for lines (95%) does not meet global threshold (100%)');
  }
};
`);
  write(gates, 'node_modules/nyc/bin/nyc.js', `#!/usr/bin/env node
const { spawnSync } = require('node:child_process');
const NYC = require('../index.js');
const args = process.argv.slice(2);
(async () => {
  const nyc = new NYC();
  if (args[0] === 'check-coverage') return nyc.checkCoverage({});
  const at = args.findIndex((arg) => !arg.startsWith('-'));
  const child = spawnSync(args[at], args.slice(at + 1), { stdio: 'inherit' });
  await nyc.getCoverageMapFromAllCoverageFiles();
  if (args.includes('--check-coverage')) await nyc.checkCoverage({});
  process.exit(process.exitCode || child.status);
})();
`);
  // karma builds karma-coverage's reporter from the plugin it requires by
  // name, through the constructor's $inject list, and the reporter checks
  // coverageReporter.check.
  write(gates, 'node_modules/karma-coverage/index.js', `function CoverageReporter(config) {
  if (config.coverageReporter && 'check' in config.coverageReporter) {
    process.exitCode = 1;
    console.error('ERROR: Coverage for branches (80%) does not meet global threshold (100%)');
  }
}
CoverageReporter.$inject = ['config'];
module.exports = { 'reporter:coverage': ['type', CoverageReporter] };
`);
  write(gates, 'node_modules/karma/bin/karma', `#!/usr/bin/env node
const plugin = require(require.resolve('karma-coverage', { paths: [process.cwd()] }));
const [, Reporter] = plugin['reporter:coverage'];
if (Reporter.$inject?.[0] !== 'config') process.exit(2);
new Reporter({ coverageReporter: { type: 'text-summary', check: { global: { branches: 100 } } } });
console.log('karma coverage report');
process.exit(process.exitCode || 0);
`);
  mkdirSync(resolve(gates, 'node_modules/.bin'), { recursive: true });
  for (const tool of ['c8', 'nyc']) {
    chmodSync(resolve(gates, `node_modules/${tool}/bin/${tool}.js`), 0o755);
    symlinkSync(`../${tool}/bin/${tool}.js`, resolve(gates, `node_modules/.bin/${tool}`));
  }
  chmodSync(resolve(gates, 'node_modules/karma/bin/karma'), 0o755);
  symlinkSync('../karma/bin/karma', resolve(gates, 'node_modules/.bin/karma'));
  const gated = supercov(gates, ['--', 'npm', 'test']);
  assert.equal(gated.status, 0, gated.output);
  assert.doesNotMatch(gated.output, /ERROR: Coverage/, gated.output);
  // Once per process: the gated run and the check-coverage command of each.
  assert.equal(gated.output.match(skipped('c8'))?.length, 2, gated.output);
  assert.equal(gated.output.match(skipped('nyc'))?.length, 2, gated.output);
  assert.equal(gated.output.match(skipped('karma-coverage'))?.length, 1, gated.output);
  assert.match(gated.output, /karma coverage report/, gated.output);
  // The copy's source map leads outside the workspace; nyc must not exclude
  // the project's file after remapping it, or its report is empty.
  assert.match(gated.output, /nyc excludeAfterRemap=false/, gated.output);
  const failing = supercov(gates, ['--', 'npm', 'test'], { FAIL_ONE: '1' });
  assert.notEqual(failing.status, 0, failing.output);

  const jestGate = resolve(temporary, 'jest-gate');
  link(jestGate, ['jest', 'jest-config'], ['jest']);
  write(jestGate, 'package.json', JSON.stringify({
    name: 'jest-gate-fixture',
    private: true,
    scripts: { test: 'jest --coverage' },
    jest: { testEnvironment: 'node', coverageThreshold: { global: { branches: 100, lines: 100 } } },
  }));
  write(jestGate, 'jest.plain.config.js', "module.exports = { testEnvironment: 'node' };\n");
  write(jestGate, 'lib/index.js', `exports.pick = function pick(value, fallback) {
  if (value && value.length > 0) return value;
  return fallback;
};
`);
  write(jestGate, 'test/index.test.js', `const { pick } = require('../lib/index.js');
test('picks', () => {
  expect(pick('a', 'b')).toBe('a');
  expect(pick('', 'b')).toBe('b');
  expect(pick(null, 'b')).toBe('b');
});
`);
  const jestRun = supercov(jestGate, ['--', 'npm', 'test']);
  assert.equal(jestRun.status, 0, jestRun.output);
  assert.doesNotMatch(jestRun.output, /does not meet/, jestRun.output);
  // Jest's own report reads what it reads without Supercov (83.33% before).
  assert.match(jestRun.output, /All files\s*\|\s*100\s*\|\s*100\s*\|/, jestRun.output);
  assert.equal(jestRun.output.match(skipped('Jest'))?.length, 1, jestRun.output);
  // No shell between here and Jest, so the JSON needs no quoting.
  const jestCli = supercov(jestGate, ['--', process.execPath, 'node_modules/jest/bin/jest.js', '--coverage',
    '--config', 'jest.plain.config.js', '--coverageThreshold', '{"global":{"lines":100,"branches":100}}']);
  assert.equal(jestCli.status, 0, jestCli.output);
  assert.equal(jestCli.output.match(skipped('Jest'))?.length, 1, jestCli.output);

  const vitestGate = resolve(temporary, 'vitest-gate');
  link(vitestGate, ['vite', 'vitest', '@vitest/coverage-v8'], ['vitest']);
  write(vitestGate, 'package.json', JSON.stringify({
    name: 'vitest-gate-fixture',
    private: true,
    type: 'module',
    scripts: { test: 'vitest run --coverage' },
  }));
  write(vitestGate, 'vitest.config.js', `export default {
  test: { coverage: { provider: 'v8', include: ['src/**'], thresholds: { 100: true, autoUpdate: true } } },
};
`);
  write(vitestGate, 'vitest.plain.config.js', `export default { test: { coverage: { provider: 'v8', include: ['src/**'] } } };
`);
  write(vitestGate, 'src/index.js', `export function pick(value, fallback) {
  if (value && value.length > 0) return value;
  return fallback;
}
`);
  write(vitestGate, 'test/index.test.js', `import { expect, test } from 'vitest';
import { pick } from '../src/index.js';
test('picks', () => {
  expect(pick('a', 'b')).toBe('a');
  expect(pick('', 'b')).toBe('b');
  expect(pick(null, 'b')).toBe('b');
});
`);
  const vitestRun = supercov(vitestGate, ['--', 'npm', 'test']);
  assert.equal(vitestRun.status, 0, vitestRun.output);
  assert.doesNotMatch(vitestRun.output, /does not meet/, vitestRun.output);
  // The suite covers every branch, and Vitest says so: Supercov's probes no
  // longer count against the user's lines (they read 87.5%). Vitest prints its
  // table's rows on Linux and only the summary under it on macOS, with or
  // without Supercov.
  assert.match(vitestRun.output, /All files\s*\|\s*100\s*\|\s*100\s*\||Branches\s+:\s+100%/, vitestRun.output);
  assert.equal(vitestRun.output.match(skipped('Vitest'))?.length, 1, vitestRun.output);
  const vitestCli = supercov(vitestGate, ['--', process.execPath, 'node_modules/vitest/vitest.mjs', 'run', '--coverage',
    '--config', 'vitest.plain.config.js', '--coverage.thresholds.lines', '100', '--coverage.thresholds.branches=100']);
  assert.equal(vitestCli.status, 0, vitestCli.output);
  assert.doesNotMatch(vitestCli.output, /does not meet/, vitestCli.output);
  assert.equal(vitestCli.output.match(skipped('Vitest'))?.length, 1, vitestCli.output);
  // Every probe form, partly covered. Each tool reads the instrumented copy
  // through its source map; the probes map to nothing, and every statement,
  // branch and function of the program keeps its place: before, Supercov's
  // `||` checks read as covered branches, a statement after a block Supercov
  // added lost its start, and `(a && b) || c` became two branches of three.
  const reports = resolve(temporary, 'reports');
  link(reports, ['jest', 'jest-config', 'vite', 'vitest', '@vitest/coverage-v8'], []);
  write(reports, 'package.json', JSON.stringify({
    name: 'reports-fixture',
    private: true,
    jest: { testEnvironment: 'node', testMatch: ['**/test/jest.test.js'], collectCoverageFrom: ['src/**'] },
  }));
  write(reports, 'vitest.config.mjs', `export default {
  test: { include: ['test/vitest.test.mjs'], coverage: { provider: 'v8', include: ['src/**'], reporter: ['text-summary'] } },
};
`);
  write(reports, 'src/mixed.js', `function mixed(options = {}, list = []) {
  const { scale = 1, label: given } = options;
  const name = options.name ?? 'anon';
  let total = 0;
  for (const item of list) {
    if (item > 0 && item < 10) total += item * scale;
    else if (item === 0) continue;
    else total -= 1;
  }
  for (const key in options.extra) total += key.length;
  let rounds = 0;
  while (rounds < 2) rounds += 1;
  try {
    if (options.fail) throw new Error('x');
  } catch (error) {
    total = -1;
  }
  switch (options.mode) {
    case 'a':
      total += 1;
      break;
    case 'b':
      total += 2;
      break;
  }
  const label = given || (total > 5 ? 'big' : 'small');
  options.seen ||= 1;
  options.count &&= options.count + 1;
  options.tag ??= 'none';
  const ok = (total > 0 && name !== 'x') || !!options.force;
  return { name, total, label, ok, len: list?.length, call: options.cb?.() };
}
const twice = (value) => value * 2;
class Box {
  constructor(v) { this.v = v ?? 0; }
  get doubled() { return twice(this.v); }
}
function unused(a) {
  return a ? 1 : 2;
}
module.exports = { mixed, Box, unused };
`);
  const body = `  assert.equal(mixed({ name: 'n', mode: 'a' }, [1, 20]).total, 1);
  assert.equal(mixed().label, 'small');
  assert.equal(mixed({ count: 2, extra: { ab: 1 }, cb: () => 3 }, [0, 3]).total, 5);
  assert.equal(new Box(2).doubled, 4);
`;
  write(reports, 'test/node.test.js', `const test = require('node:test');
const assert = require('node:assert/strict');
const { mixed, Box } = require('../src/mixed.js');
test('mixed', () => {
${body}});
`);
  write(reports, 'test/jest.test.js', `const assert = require('node:assert/strict');
const { mixed, Box } = require('../src/mixed.js');
test('mixed', () => {
${body}});
`);
  write(reports, 'test/vitest.test.mjs', `import assert from 'node:assert/strict';
import { test } from 'vitest';
import { mixed, Box } from '../src/mixed.js';
test('mixed', () => {
${body}});
`);
  const nodeRow = (output) => /^\S*\s+mixed\.js\s*\|(.*)$/m.exec(output)?.[1].trim();
  const summary = (output) => output.match(/^(?:Statements|Branches|Functions|Lines)\s+:.*$/gm)?.join('\n');
  for (const [tool, command, read] of [
    ['node', [process.execPath, '--test', '--experimental-test-coverage', 'test/node.test.js'], nodeRow],
    ['jest', [process.execPath, 'node_modules/jest/bin/jest.js', '--coverage', '--coverageReporters=text-summary'], summary],
    ['vitest', [process.execPath, 'node_modules/vitest/vitest.mjs', 'run', '--coverage'], summary],
  ]) {
    const plain = spawnSync(command[0], command.slice(1), { cwd: reports, encoding: 'utf8', env: { ...process.env, CI: '1', NO_COLOR: '1' } });
    const plainOutput = `${plain.stdout}\n${plain.stderr}`;
    assert.equal(plain.status, 0, plainOutput);
    const expected = read(plainOutput);
    assert.ok(expected, `${tool} reports without Supercov:\n${plainOutput}`);
    const measured = supercov(reports, ['--', ...command]);
    assert.equal(measured.status, 0, measured.output);
    assert.equal(read(measured.output), expected, `${tool} under Supercov:\n${measured.output}`);
  }
  // Node's thresholds judge that same report: met at its figures, missed a
  // point above, as without Supercov.
  const [lines, branches, functions] = nodeRow(spawnSync(process.execPath,
    ['--test', '--experimental-test-coverage', 'test/node.test.js'], { cwd: reports, encoding: 'utf8' }).stdout)
    .split('|').map((cell) => Math.floor(Number.parseFloat(cell)));
  const thresholds = (extra) => [process.execPath, '--test', '--experimental-test-coverage',
    `--test-coverage-lines=${lines + extra}`, `--test-coverage-branches=${branches}`, `--test-coverage-functions=${functions}`, 'test/node.test.js'];
  const met = supercov(reports, ['--', ...thresholds(0)]);
  assert.equal(met.status, 0, met.output);
  const missed = supercov(reports, ['--', ...thresholds(1)]);
  assert.notEqual(missed.status, 0, missed.output);
  assert.match(missed.output, /line coverage does not meet threshold/, missed.output);
  console.log('top package failure classes pass through the public command');
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
