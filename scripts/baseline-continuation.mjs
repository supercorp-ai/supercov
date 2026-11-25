// A pass continues from a merged baseline of the same checkout.
//
//   node scripts/baseline-continuation.mjs
//
// A Playwright suite is measured in two parts, on two "machines", each with
// a command of its own, and the parts are merged: the weekly baseline. A
// mutation pass whose command runs some of the suite's spec files is made
// from the merged run with those files' tests alone, and measures nothing;
// its verdicts are those a pass that measured first gives. Once the
// checkout changes, the baseline is of another checkout, and the pass
// measures. Prints what continuing saved.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePort } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `baseline-continuation-${process.pid}-${Date.now()}`);

const port = await freePort();
// Each test asks the app's server, which runs the source the mutants are of.
const ask = (what, amount) => `(await (await request.get('http://127.0.0.1:${port}/${what}/${amount}')).text())`;
const spec = (name, body) => `import { expect, test } from '@playwright/test';\n\ntest('${name}', async ({ request }) => {\n${body}\n});\n`;
const files = {
  'package.json': '{ "name": "baseline-continuation", "private": true, "type": "module" }\n',
  'src/price.js': "export function price(amount) {\n  return amount > 100 ? amount - 10 : amount;\n}\nexport function label(amount) {\n  return amount >= 50 ? 'large' : 'small';\n}\n",
  'server.mjs': "import { createServer } from 'node:http';\nimport { price, label } from './src/price.js';\ncreateServer((request, response) => {\n  const [, what, amount] = request.url.split('/');\n  response.end(what === 'ready' ? 'ready' : String(what === 'price' ? price(Number(amount)) : label(Number(amount))));\n}).listen(Number(process.env.PORT));\n",
  'playwright.config.js': `export default {\n  testDir: 'tests/e2e',\n  workers: 1,\n  reporter: 'line',\n  webServer: { command: 'node server.mjs', env: { PORT: '${port}' }, url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false },\n};\n`,
  'tests/e2e/price.spec.js': spec('prices', `  expect(${ask('price', 500)}).toBe('490');\n  expect(${ask('price', 5)}).toBe('5');`),
  'tests/e2e/label.spec.js': spec('labels', `  expect(${ask('label', 80)}).toBe('large');\n  expect(${ask('label', 10)}).toBe('small');`),
  // A test that fails in the baseline: no candidate, as in any run.
  'tests/e2e/broken.spec.js': spec('broken', "  expect(1).toBe(2);"),
  'tests/e2e/both.spec.js': spec('both', `  expect(${ask('label', 200)}).toBe('large');\n  expect(${ask('price', 120)}).toBe('110');`),
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const runs = () => JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs;
const timed = (args) => {
  const started = Date.now();
  const result = run(binary, args);
  return { ...result, seconds: (Date.now() - started) / 1000 };
};
// What the pass decided, mutant by mutant.
const verdicts = () => {
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'mutants', 'src/price.js', '--json', '--limit', '100']).stdout);
  return Object.fromEntries(listed.data.mutants.map((mutant) => [mutant.id, mutant.strong?.status ?? null]));
};
// The pass names its spec files by path and by name alone.
const command = ['npx', 'playwright', 'test', 'tests/e2e/price.spec.js', 'both.spec.js'];
const pass = ['mutants', '--file', 'src/price.js', '--json', '--', ...command];

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  for (const args of [['init', '-q'], ['add', '.'], ['-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-qm', 'suite']]) {
    assert.equal(run('git', args).status, 0);
  }
  // The baseline: two parts, each its own command, merged; a test of the
  // first fails.
  const parts = [['tests/e2e/price.spec.js', 'tests/e2e/label.spec.js', 'tests/e2e/broken.spec.js'], ['tests/e2e/both.spec.js']];
  for (const [index, part] of parts.entries()) {
    const measured = run(binary, ['--', 'npx', 'playwright', 'test', ...part]);
    assert.equal(measured.status, index === 0 ? 1 : 0, measured.stdout + measured.stderr);
  }
  const merged = run(binary, ['merge']);
  assert.equal(merged.status, 0, merged.stdout + merged.stderr);
  const baseline = runs()[0].id;
  // The pass is made from it, measuring nothing.
  const continued = timed(pass);
  assert.match(continued.stderr, new RegExp(`not measuring: run ${baseline} of this checkout holds the tests the command runs \\(2 test\\(s\\) in 2 of its 4 test file\\(s\\)\\)`, 'u'), continued.stderr);
  assert.doesNotMatch(continued.stderr, /measuring first/u, continued.stderr);
  const fromBaseline = verdicts();
  const decided = Object.values(fromBaseline);
  assert.ok(decided.includes('killed') && decided.every((status) => status), JSON.stringify(fromBaseline));
  // The same pass on a store without the baseline measures first, and
  // decides alike.
  rmSync(resolve(root, '.supercov'), { recursive: true, force: true });
  const measuredFirst = timed(pass);
  assert.match(measuredFirst.stderr, /measuring first/u, measuredFirst.stderr);
  assert.deepEqual(fromBaseline, verdicts());
  // A suite split across machines, each measuring the pass's own command:
  // the merge of their runs holds all of its tests. A run that cannot be
  // made from it is said, and the pass ends.
  rmSync(resolve(root, '.supercov'), { recursive: true, force: true });
  const shards = [0, 1].map(() => {
    assert.equal(run(binary, ['--', ...command]).status, 0);
    return runs()[0].id;
  });
  const split = run(binary, ['merge', ...shards]);
  assert.equal(split.status, 0, split.stdout + split.stderr);
  const unmade = run(binary, pass, { SUPERCOV_TEST_FAULT: 'fs.rename#lifecycle.publish-run-with-fault.rename' });
  assert.equal(unmade.status, 2, unmade.stderr);
  assert.match(unmade.stderr, /could not make this pass's run from run_\w+: Could not publish the run/u, unmade.stderr);
  const whole = run(binary, pass);
  assert.match(whole.stderr, /holds the tests the command runs \(2 test\(s\) in 2 of its 2 test file\(s\)\)/u, whole.stderr);
  assert.deepEqual(fromBaseline, verdicts());
  // A baseline whose command failed otherwise than in a test (here, one
  // part's exit status says so) is no source, nor is a part that lacks one
  // of the command's spec files: the pass measures.
  rmSync(resolve(root, '.supercov'), { recursive: true, force: true });
  const passing = [['tests/e2e/price.spec.js', 'tests/e2e/label.spec.js'], ['tests/e2e/both.spec.js']].map((part) => {
    assert.equal(run(binary, ['--', 'npx', 'playwright', 'test', ...part]).status, 0);
    return runs()[0].id;
  });
  const record = resolve(root, '.supercov/runs', passing[1], 'run.json');
  writeFileSync(record, JSON.stringify({ ...JSON.parse(readFileSync(record, 'utf8')), testExitCode: 1 }));
  assert.equal(run(binary, ['merge', ...passing]).status, 0);
  const crashed = run(binary, pass);
  assert.match(crashed.stderr, /measuring first/u, crashed.stderr);
  // A changed checkout is not the baseline's: the pass measures.
  for (const [index, part] of parts.entries()) {
    assert.equal(run(binary, ['--', 'npx', 'playwright', 'test', ...part]).status, index === 0 ? 1 : 0);
  }
  assert.equal(run(binary, ['merge']).status, 0);
  writeFileSync(resolve(root, 'src/price.js'), `${files['src/price.js']}export const changed = 1;\n`);
  const changed = run(binary, pass);
  assert.match(changed.stderr, /measuring first/u, changed.stderr);
  console.log(`[baseline-continuation] a pass made from the merged baseline measured nothing: ${continued.seconds.toFixed(1)} s, against ${measuredFirst.seconds.toFixed(1)} s measuring first, with the same verdicts`);
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'baseline-continuation-'))) rmSync(root, { recursive: true, force: true });
}
