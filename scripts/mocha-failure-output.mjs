// A Mocha failure reads as it does without Supercov: in a CommonJS project,
// and in a TypeScript project that ts-node compiles as it runs. Mocha, ts-node
// and TypeScript 5 are installed into the temporary project from the
// registry; the repository's own TypeScript has no compiler API for ts-node.
//
// Before this, with plain JavaScript:
//   - an application frame printed the project's whole path, since Mocha takes
//     its working directory off a frame and under Supercov that is the copy;
//   - a failed `node:assert` printed the function Supercov had added, Node's
//     `AsyncLocalStorage.run` and the author's function, for one line;
//   - every stack that began in a timer ended in a frame of Node's hooks.
// And with ts-node an application frame read `src/crypto.ts:37:10` for line
// 8, because TypeScript reads no source map in the text it compiles.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const windows = process.platform === 'win32';
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-mocha-output-'));
const project = resolve(temporary, 'project');

const source = (typed) => [
  typed ? 'export interface Sealed {' : '// What a sealed message holds.',
  typed ? '  key: string;' : '//   key: the key it was sealed with',
  typed ? '  body: string;' : '//   body: the text, upper case',
  typed ? '}' : '',
  '',
  typed ? 'export function seal(key: string, body: string): Sealed {' : 'function seal(key, body) {',
  '  if (key.length < 4) {',
  '    throw new Error("key too short");',
  '  }',
  '  return { key, body: body.toUpperCase() };',
  '}',
  typed ? '' : 'module.exports = { seal };',
  '',
].join('\n');
const tests = (load) => [
  load,
  '',
  'describe("seal", () => {',
  '  it("seals a body", () => {',
  '    assert.equal(seal("abcd", "x").body, "X");',
  '  });',
  '',
  '  it("refuses a short key", () => {',
  '    const sealed = seal("ab", "x");',
  '    assert.equal(sealed.body, "X");',
  '  });',
  '',
  '  it("compares", () => {',
  '    assert.equal(seal("abcd", "y").body, "Z");',
  '  });',
  '',
  '  it("fails later", (done) => {',
  '    setTimeout(() => {',
  '      try {',
  '        assert.deepEqual(seal("abcd", "z"), { key: "abcd", body: "z" });',
  '        done();',
  '      } catch (error) {',
  '        done(error);',
  '      }',
  '    }, 1);',
  '  });',
  '});',
  '',
].join('\n');

try {
  mkdirSync(resolve(project, 'src'), { recursive: true });
  mkdirSync(resolve(project, 'test'), { recursive: true });
  writeFileSync(resolve(project, 'package.json'), JSON.stringify({ name: 'mocha-output', private: true }));
  writeFileSync(resolve(project, '.gitignore'), 'node_modules\n.supercov\n');
  writeFileSync(resolve(project, 'tsconfig.json'), JSON.stringify({
    compilerOptions: { target: 'ES2022', module: 'commonjs', strict: true, esModuleInterop: true, types: ['mocha', 'node'] },
  }));
  writeFileSync(resolve(project, 'src/crypto.js'), source(false));
  writeFileSync(resolve(project, 'test/crypto.test.js'),
    tests('const assert = require("node:assert/strict");\nconst { seal } = require("../src/crypto.js");'));
  writeFileSync(resolve(project, 'src/sealed.ts'), source(true));
  writeFileSync(resolve(project, 'test/sealed.test.ts'),
    tests('import assert from "node:assert/strict";\nimport { seal } from "../src/sealed";'));
  const install = spawnSync(windows ? 'npm.cmd' : 'npm',
    ['install', '--no-audit', '--no-fund', '--silent', 'mocha@11', 'ts-node@10', 'typescript@5', '@types/mocha@10', '@types/node@22'],
    { cwd: project, encoding: 'utf8', shell: windows });
  assert.equal(install.status, 0, `installing mocha: ${install.stdout}\n${install.stderr}`);
  spawnSync('git', ['init', '-q'], { cwd: project });

  // What a failure prints: its messages, its diff and its frames. Not the
  // lines that carry a duration, or Supercov's own notes.
  const failure = (command, args) => {
    const result = spawnSync(command, args, {
      cwd: project,
      encoding: 'utf8',
      env: { ...process.env, NO_COLOR: '1', FORCE_COLOR: '0', CI: '1' },
    });
    assert.notEqual(result.status, 0, result.stdout + result.stderr);
    const lines = `${result.stdout}${result.stderr}`
      .split('\n')
      .map((line) => line.trimEnd().replace(/ \(\d+ms\)$/u, ''))
      .filter((line) => line !== '' && !/^\[(?:supercov|coverage)\]|^\s+\d+ (?:passing|failing)/u.test(line));
    assert.ok(lines.some((line) => /^\s+3 failing|3\) seal/u.test(line)) || lines.length >= 20, result.stdout + result.stderr);
    return lines;
  };
  const mocha = resolve(project, 'node_modules/mocha/bin/mocha.js');
  for (const [name, args, frames] of [
    ['JavaScript', ['test/crypto.test.js'], [
      /^\s+at seal \(src[\\/]crypto\.js:8:11\)$/u,
      /^\s+at Context\.<anonymous> \(test[\\/]crypto\.test\.js:15:12\)$/u,
    ]],
    ['ts-node', ['-r', 'ts-node/register', 'test/sealed.test.ts'], [
      /^\s+at seal \(src[\\/]sealed\.ts:8:11\)$/u,
      /^\s+at Context\.<anonymous> \(test[\\/]sealed\.test\.ts:15:12\)$/u,
    ]],
  ]) {
    const plain = failure(process.execPath, [mocha, ...args]);
    for (const frame of frames)
      assert.ok(plain.some((line) => frame.test(line)), `${name}: ${frame}\n${plain.join('\n')}`);
    const measured = failure(binary, ['--', process.execPath, mocha, ...args]);
    assert.deepEqual(measured, plain, name);
  }
  console.log('[mocha-failure-output] a Mocha failure reads as it does without Supercov, in JavaScript and under ts-node');
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
