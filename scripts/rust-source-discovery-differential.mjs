import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

import { discoverSourceScope } from '../dist/sourceDiscovery.js';

const root = resolve(import.meta.dirname, '..');
const binary = resolve(root, 'target/debug/supercov');
const temporary = [];

function repository(files) {
  const directory = mkdtempSync(resolve(tmpdir(), 'supercov-rust-source-diff-'));
  temporary.push(directory);
  for (const [file, contents] of Object.entries(files)) {
    const path = resolve(directory, file);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, contents);
  }
  return directory;
}

function rust(directory, configuredRoots) {
  const result = spawnSync(binary, ['__discover-source'], {
    input: JSON.stringify({
      root: directory,
      ...(configuredRoots ? { configuredRoots } : {}),
    }),
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return JSON.parse(result.stdout);
}

const cases = [
  {
    name: 'automatic workspace',
    root: repository({
      'package.json': JSON.stringify({ workspaces: ['packages/*'] }),
      'src/index.ts': 'export const root = true',
      'lib/helper.js': 'export const helper = true',
      'src/index.test.ts': 'test()',
      'scripts/release.mjs': 'release()',
      'vite.config.ts': 'export default {}',
      '.eslintrc.cjs': 'module.exports = {}',
      'orphan.ts': 'export const orphan = true',
      'packages/ui/package.json': JSON.stringify({ module: './src/index.ts' }),
      'packages/ui/src/index.ts': 'export const ui = true',
      'packages/ui/tests/ui.spec.ts': 'test()',
      'dist/generated.js': 'generated()',
    }),
  },
  {
    name: 'explicit roots',
    root: repository({
      'package.json': '{}',
      'product/main.ts': 'product()',
      'orphan.ts': 'outside()',
    }),
    configuredRoots: ['product'],
  },
  {
    name: 'JSONC TypeScript default root',
    root: repository({
      'package.json': JSON.stringify({ main: './dist/index.js' }),
      'tsconfig.json': '{\n // comment\n "compilerOptions": { "target": "es2022", },\n}',
      'events.ts': 'event()',
      'library.ts': 'library()',
      'library.test.ts': 'test()',
    }),
  },
  ...['generic-playwright', 'generic-node', 'generic-esbuild', 'generic-webpack', 'generic-swc']
    .map((fixture) => ({
      name: fixture,
      root: resolve(root, 'tests/fixtures', fixture),
    })),
];

try {
  for (const fixture of cases) {
    const expected = discoverSourceScope(fixture.root, fixture.configuredRoots);
    const actual = rust(fixture.root, fixture.configuredRoots);
    assert.deepEqual(actual, expected, `${fixture.name} source discovery differs`);
  }
  console.log(
    `[rust-source-discovery-differential] ${cases.length} synthetic and real project shapes have exact source-scope parity`,
  );
} finally {
  for (const directory of temporary) rmSync(directory, { recursive: true, force: true });
}
