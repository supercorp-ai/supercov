import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

// npm can leave jest-config only under jest-cli (and @jest/core), where the
// project root does not resolve it; the project's configuration must still be
// read, or Jest's defaults run tests the configuration leaves out.
test("the project's Jest configuration is read through the running Jest when the root cannot see jest-config", t => {
  const project = mkdtempSync(resolve(tmpdir(), 'supercov-jest-config-'));
  t.after(() => rmSync(project, { recursive: true, force: true }));
  const write = (path, text) => {
    mkdirSync(resolve(project, path, '..'), { recursive: true });
    writeFileSync(resolve(project, path), text);
  };
  write('package.json', '{"name":"app","private":true}');
  write('node_modules/jest/package.json', '{"name":"jest","version":"30.0.5","bin":"bin/jest.js"}');
  write('node_modules/jest/bin/jest.js', '');
  write('node_modules/jest-cli/package.json', '{"name":"jest-cli","version":"30.0.5"}');
  write('node_modules/jest-cli/node_modules/jest-config/package.json', '{"name":"jest-config","version":"30.0.5","main":"index.js"}');
  write(
    'node_modules/jest-cli/node_modules/jest-config/index.js',
    "exports.readInitialOptions = async () => ({ config: { rootDir: process.cwd(), testPathIgnorePatterns: ['<rootDir>/tests/e2e/'] }, configPath: null });",
  );
  const config = pathToFileURL(resolve(import.meta.dirname, '../../runtime/javascript/jest.config.mjs')).href;
  const script = `
    process.argv[1] = ${JSON.stringify(resolve(project, 'node_modules/jest/bin/jest.js'))};
    const { default: read } = await import(${JSON.stringify(config)});
    const config = await read();
    console.log(JSON.stringify(config.testPathIgnorePatterns ?? null));
  `;
  const ran = spawnSync(process.execPath, ['--input-type=module', '-e', script], { cwd: project, encoding: 'utf8' });
  assert.equal(ran.status, 0, ran.stderr);
  assert.doesNotMatch(ran.stderr, /configuration was not read/);
  assert.deepEqual(JSON.parse(ran.stdout.trim()), ['<rootDir>/tests/e2e/']);
});
