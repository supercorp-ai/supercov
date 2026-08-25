import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

import { discoverCoverageProject } from '../dist/project.js';

const workspace = resolve(import.meta.dirname, '..');
const binary = resolve(workspace, 'target/debug/supercov');
const temporary = [];

function repository(files) {
  const root = mkdtempSync(resolve(tmpdir(), 'supercov-rust-project-diff-'));
  temporary.push(root);
  for (const [file, contents] of Object.entries(files)) {
    const path = resolve(root, file);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, contents);
  }
  return root;
}

function rust(root, environment = {}, command = []) {
  const result = spawnSync(binary, ['__discover-project'], {
    input: JSON.stringify({ root, environment, command }),
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return JSON.parse(result.stdout);
}

const cases = [
  {
    name: 'Vite and Playwright',
    root: repository({
      'package.json': JSON.stringify({ scripts: { build: 'vite build' } }),
      'src/main.ts': 'export const ready = true',
      'playwright.config.ts': 'export default {}',
      'vitest.config.ts': 'export default {}',
      'tests/example.spec.ts': "import { test } from '@playwright/test'",
    }),
  },
  {
    name: 'source-executing node:test',
    root: repository({
      'package.json': JSON.stringify({
        scripts: { build: 'tsc -p tsconfig.build.json', test: 'node --test tests/*.test.ts' },
      }),
      'src/index.ts': 'export const ready = true',
      'tests/index.test.ts': "import { test } from 'node:test'",
    }),
    command: ['npm', 'test'],
  },
  {
    name: 'compiled-output Jest',
    root: repository({
      'package.json': JSON.stringify({ scripts: { build: 'tsc', test: 'jest --runInBand' } }),
      'src/index.ts': 'export const ready = true',
      'jest.config.js': 'module.exports = {}',
      'test/index.test.js': "require('../dist/index.js')",
    }),
    command: ['npm', 'test'],
  },
  {
    name: 'custom Playwright fixture',
    root: repository({
      'package.json': JSON.stringify({ scripts: { build: 'vite build' } }),
      'app/root.tsx': 'export default null',
      'tests/nested/playwright.browser.config.ts': 'export default {}',
      'tests/example.spec.ts':
        "import { browserTest as test, expect, fixtureValue } from '@acme/browser-fixtures'",
    }),
  },
  {
    name: 'inferred build environment',
    root: repository({
      'package.json': JSON.stringify({
        scripts: { build: 'vite build', 'test:isolated': 'node tools/run-suite.js' },
      }),
      'app/root.ts': 'export const ready = true',
      'vite.config.ts':
        "const isolated = process.env.TEST_ISOLATED === 'true'; export default { isolated }",
    }),
    command: ['npm', 'run', 'test:isolated'],
  },
  ...['generic-playwright', 'generic-node', 'generic-esbuild', 'generic-webpack', 'generic-swc']
    .map((fixture) => ({
      name: fixture,
      root: resolve(workspace, 'tests/fixtures', fixture),
      command: ['npm', 'test'],
    })),
];

try {
  for (const fixture of cases) {
    const environment = fixture.environment ?? {};
    const command = fixture.command ?? [];
    const expected = discoverCoverageProject(fixture.root, environment, command);
    const actual = rust(fixture.root, environment, command);
    assert.deepEqual(actual, expected, `${fixture.name} project discovery differs`);
  }
  console.log(
    `[rust-project-discovery-differential] ${cases.length} synthetic and real project shapes have exact project/build/runner parity`,
  );
} finally {
  for (const root of temporary) rmSync(root, { recursive: true, force: true });
}
