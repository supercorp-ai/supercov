import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
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

// Jest 27 and 28 have no readInitialOptions: the configuration is read with
// the two steps of their build that their package's exports hide.
test("the project's Jest configuration is read on Jest before 29.3", t => {
  const project = mkdtempSync(resolve(tmpdir(), 'supercov-jest-config-'));
  t.after(() => rmSync(project, { recursive: true, force: true }));
  const write = (path, text) => {
    mkdirSync(resolve(project, path, '..'), { recursive: true });
    writeFileSync(resolve(project, path), text);
  };
  write('package.json', '{"name":"app","private":true}');
  write('jest.config.js', "module.exports = { roots: ['<rootDir>/test'] };");
  write('node_modules/jest-config/package.json', JSON.stringify({
    name: 'jest-config', version: '28.1.3', main: 'build/index.js',
    exports: { '.': './build/index.js', './package.json': './package.json' },
  }));
  write('node_modules/jest-config/build/index.js', "exports.isJSONString = text => text.startsWith('{');");
  write(
    'node_modules/jest-config/build/resolveConfigPath.js',
    "const path = require('path'); exports.default = (given, cwd, skip) => { if (!skip) throw new Error('multiple'); return path.resolve(cwd, given === cwd ? 'jest.config.js' : given); };",
  );
  write(
    'node_modules/jest-config/build/readConfigFileAndSetRootDir.js',
    "const path = require('path'); exports.default = async file => ({ ...require(file), rootDir: path.dirname(file) });",
  );
  const config = pathToFileURL(resolve(import.meta.dirname, '../../runtime/javascript/jest.config.mjs')).href;
  const read = environment => {
    const script = `
      const { default: read } = await import(${JSON.stringify(config)});
      const config = await read();
      console.log(JSON.stringify({ roots: config.roots ?? null, rootDir: config.rootDir }));
    `;
    const ran = spawnSync(process.execPath, ['--input-type=module', '-e', script], { cwd: project, encoding: 'utf8', env: { ...process.env, ...environment } });
    assert.equal(ran.status, 0, ran.stderr);
    assert.doesNotMatch(ran.stderr, /configuration was not read/);
    return JSON.parse(ran.stdout.trim());
  };
  assert.deepEqual(read({}), { roots: ['<rootDir>/test'], rootDir: realpathSync(project) });
  // A configuration given as JSON on the command line.
  assert.deepEqual(read({ SUPERCOV_ORIGINAL_JEST_CONFIG: '{"roots":["<rootDir>/spec"]}' }).roots, ['<rootDir>/spec']);
});
