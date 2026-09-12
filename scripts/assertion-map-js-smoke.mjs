import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, writeFileSync, symlinkSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const write = (path, value) => writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);

// Used both from a development checkout and from the real packed npm install.
// Authored claims below describe this fixture only; no inference runs in product.
export function assertionMapSmoke({ root, launcher, env, runner = 'node', typescript = false, commonjs = false }) {
  mkdirSync(resolve(root, 'src'), { recursive: true });
  mkdirSync(resolve(root, 'tests'), { recursive: true });
  const ext = typescript ? 'ts' : commonjs ? 'cjs' : 'js';
  const file = `src/value.${ext}`;
  const testFile = `tests/value.test.${ext}`;
  const application = `${commonjs ? '' : 'export '}function value(input${typescript ? ': number' : ''}) {\n  if (input > 0) {\n    return input + 1;\n  }\n  return 0;\n}\n${commonjs ? '' : 'export '}async function fail() {\n  throw new Error('boom');\n}\n${commonjs ? 'module.exports = { value, fail };\n' : ''}`;
  writeFileSync(resolve(root, file), application);
  const packageData = { name: 'assertion-map-js-pilot', private: true, type: commonjs ? 'commonjs' : 'module' };
  if (runner !== 'node') {
    packageData.scripts = { test: runner === 'vitest' ? 'vitest run' : runner === 'jest' ? 'jest --runInBand' : 'playwright test' };
    mkdirSync(resolve(root, 'node_modules/.bin'), { recursive: true });
    const dependencies = runner === 'vitest' ? ['vitest', 'vite'] : runner === 'jest' ? ['jest', '@jest/globals'] : ['@playwright/test'];
    for (const dependency of dependencies) {
      mkdirSync(resolve(root, 'node_modules', dependency, '..'), { recursive: true });
      symlinkSync(resolve(repository, 'node_modules', dependency), resolve(root, 'node_modules', dependency), process.platform === 'win32' ? 'junction' : 'dir');
    }
    // A plain launcher is portable to Windows, where .bin links are .cmd files.
    const entry = runner === 'vitest' ? 'vitest/vitest.mjs' : runner === 'jest' ? 'jest/bin/jest.js' : '@playwright/test/cli.js';
    writeFileSync(resolve(root, 'runner.cjs'), `require('node:child_process').spawnSync(process.execPath, [require('node:path').join(__dirname, 'node_modules', ${JSON.stringify(entry)}), ...process.argv.slice(2)], {stdio:'inherit'}).status === 0 || process.exit(1);\n`);
    // Discovery follows the npm script's runner executable, so preserve its
    // standard name rather than depending on a PATH entry outside this project.
    packageData.scripts.test = runner === 'vitest' ? 'vitest run' : runner === 'jest' ? 'jest --runInBand' : 'playwright test';
    if (process.platform === 'win32') {
      writeFileSync(resolve(root, `node_modules/.bin/${runner}.cmd`), `@node "%~dp0\\..\\..\\runner.cjs" %*\r\n`);
    } else {
      symlinkSync(resolve(repository, 'node_modules/.bin', runner), resolve(root, 'node_modules/.bin', runner));
    }
  }
  if (runner === 'playwright') {
    mkdirSync(resolve(root, 'node_modules/@acme/fixtures'), { recursive: true });
    write(resolve(root, 'node_modules/@acme/fixtures/package.json'), { name: '@acme/fixtures', type: 'module', exports: './index.js' });
    writeFileSync(resolve(root, 'node_modules/@acme/fixtures/index.js'), "export { test, expect } from '@playwright/test';\n");
  }
  write(resolve(root, 'package.json'), packageData);
  if (runner === 'jest') writeFileSync(resolve(root, 'jest.config.cjs'), "module.exports = { testEnvironment: 'node', testMatch: ['**/tests/*.test.cjs'] };\n");
  if (runner === 'playwright') writeFileSync(resolve(root, 'playwright.config.mjs'), "export default { testDir: './tests', workers: 2, reporter: 'line' };\n");
  const imports = commonjs
    ? `const { value, fail } = require('../${file}');\n${runner === 'node' ? "const test = require('node:test'); const assert = require('node:assert/strict');" : "const { test, expect } = require('@jest/globals');"}\n`
    : `import { value, fail } from '../${file}';\n${runner === 'node' ? "import test from 'node:test'; import assert from 'node:assert/strict';" : `import { test, expect } from '${runner === 'playwright' ? '@acme/fixtures' : runner}';`}\n`;
  const parameterized = runner === 'vitest' || runner === 'jest';
  const assertions = runner === 'node'
    ? ["assert.equal(await Promise.resolve(actual), 3)", "assert.deepStrictEqual(value(0), 0)", "await assert.rejects(fail, /boom/)"]
    : [parameterized ? "expect(await Promise.resolve(actual)).toBe(expected)" : "expect(await Promise.resolve(actual)).toBe(3)", "expect(value(0)).toEqual(0)", "await expect(fail()).rejects.toThrow('boom')"];
  const source = `${imports}${parameterized ? "test.each([[1, 2], [2, 3]])('positive %s', async (input, expected) => { const actual = value(input);" : "test('positive', async () => { const actual = value(2);"} const label = '🧪'; ${assertions[0]}; });\ntest('zero', () => { ${assertions[1]}; });\ntest('rejects', async () => { ${assertions[2]}; });\n`;
  writeFileSync(resolve(root, testFile), source);
  const invoke = (...args) => {
    const r = spawnSync(process.execPath, [launcher, ...args], { cwd: root, env, encoding: 'utf8', timeout: 120_000, maxBuffer: 4 * 1024 * 1024 });
    assert.ifError(r.error);
    return r;
  };
  const ok = (...args) => { const r = invoke(...args); assert.equal(r.status, 0, `${args.join(' ')}\n${r.stdout}\n${r.stderr}`); return r; };
  const data = (...args) => JSON.parse(ok(...args, '--json').stdout).data;
  ok('--', ...(runner === 'node' ? [process.execPath, '--test', testFile] : ['npm', 'test']));
  const run = data('runs').runs[0].id;
  const query = (...args) => data('runs', run, 'assertions', ...args);
  const init = query('init');
  const map = JSON.parse(readFileSync(init.map, 'utf8'));
  assert.equal(map.assertions.length, 3, `${runner}/${ext}: ${JSON.stringify(map)}`);
  const statements = query('report', '--view', 'statements', '--limit', '100').items;
  const observed = query('report', '--view', 'assertions', '--file', testFile).items;
  assert.equal(query('inventory', '--file', testFile).pagination.total, 3);
  assert(query('files', '--limit', '100').items.some(f => f.file === file));
  assert(observed.every((a, i) => a.observedPassingTests.length === (parameterized && i === 0 ? 2 : 1)), `${runner}/${ext}: ${JSON.stringify(observed)}`);
  assert.equal(invoke('runs', run, 'assertions', 'check', '--require-complete').status, 2, 'unmapped inventory must fail a completion gate');
  const credit = ['return input + 1;', 'return 0;', "throw new Error('boom');"];
  for (const [index, a] of map.assertions.entries()) {
    const statement = statements.find(s => s.at?.text === credit[index]);
    assert(statement, `${runner}/${ext}: missing source statement ${credit[index]}: ${JSON.stringify(statements)}`);
    a.analysis = 'mapped';
    a.observes = [index === 2 ? 'Rejection message matches boom' : parameterized && index === 0 ? 'Result equals the expected number for each parameterized case' : `Result equals ${index === 0 ? 3 : 0}`];
    a.flows = [{ id: 'result', explanation: index === 2 ? 'The thrown error becomes the rejection inspected by this matcher.' : 'The returned number flows through the test call to this equality assertion.', nodes: [{ id: 'result', at: statement.at }], countsAsAsserted: ['result'], watch: [{ kind: 'file', file }, { kind: 'file', file: testFile }] }];
  }
  write(init.map, map);
  query('validate');
  assert.equal(invoke('runs', run, 'assertions', 'check').status, 2, 'unreviewed edits must fail');
  query('review', '--all');
  const report = query('check', '--require-complete', '--require-observed', '--min', '1');
  assert.equal(report.summary.statements.asserted, 3, JSON.stringify(report));
  assert.equal(invoke('runs', run, 'assertions', 'check', '--min', '100').status, 2, 'unclaimed condition remains outside assertion credit');
  const schema = JSON.parse(ok('assertions', 'schema').stdout);
  assert.equal(schema.additionalProperties, false);
  data('assertions', 'validate', '--file', init.map);
  const revision = query().revision;
  const bad = structuredClone(map); bad.assertions[0].flows[0].countsAsAsserted = true;
  write(init.map, bad);
  const invalid = JSON.parse(invoke('runs', run, 'assertions', 'validate', '--json').stdout).data;
  assert.equal(invalid.valid, false);
  assert.equal(invalid.errors[0].pointer, '/assertions/0/flows/0/countsAsAsserted');
  write(init.map, map);
  assert.equal(query().revision, revision);
  writeFileSync(resolve(root, file), application.replace('input + 1', '1 + input'));
  assert.equal(invoke('runs', run, 'assertions', 'check').status, 2, 'archived score cannot endorse changed source');
  query('check', '--archived', '--require-complete');
  writeFileSync(resolve(root, file), application);
  console.log(JSON.stringify({ runner, language: typescript ? 'typescript' : 'javascript', module: commonjs ? 'commonjs' : 'esm', assertions: 3, creditedStatements: 3, diagnostics: 'syntax, review, completion, threshold, stale checkout' }));
  return { run, root, invoke, ok, query, map, mapFile: init.map, testFile, source };
}
