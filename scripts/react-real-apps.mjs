import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
const repository = resolve(import.meta.dirname, '..');
const binary =
  process.env.SUPERCOV_BINARY ??
  resolve(
  repository,
  `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`,
);
const audit =
  process.env.SUPERCOV_REACT_AUDIT_DIR ??
  mkdtempSync(join(tmpdir(), 'supercov-real-react-'));
mkdirSync(audit, { recursive: true });
const tooling = join(audit, 'tooling');
mkdirSync(tooling, { recursive: true });
const env = {
  ...process.env,
  CI: '1',
  pnpm_config_verify_deps_before_run: 'false',
};
function run(cwd, command, args, label, extra = {}) {
  const result = spawnSync(command, args, {
    cwd,
    env: { ...env, ...extra },
    encoding: 'utf8',
    timeout: 600000,
    maxBuffer: 30 * 1024 * 1024,
  });
  if (label)
    writeFileSync(join(audit, `${label}.log`), result.stdout + result.stderr);
  assert.ifError(result.error);
  assert.equal(
    result.status,
    0,
    `${label}: ${result.stdout}\n${result.stderr}`,
  );
  return result.stdout;
}
function clone(name, url, revision) {
  const dir = join(audit, name);
  mkdirSync(dir);
  run(dir, 'git', ['init']);
  run(dir, 'git', ['remote', 'add', 'origin', url]);
  run(dir, 'git', ['fetch', '--depth', '1', 'origin', revision]);
  run(dir, 'git', ['checkout', '--detach', 'FETCH_HEAD']);
  return dir;
}
function verify(cwd, name, command, args, expected, extra = {}) {
  run(cwd, command, args, `${name}-baseline`, extra);
  run(cwd, binary, ['--', command, ...args], `${name}-supercov`, extra);
  const raw = run(cwd, binary, ['runs', 'latest', '--json']);
  writeFileSync(join(audit, `${name}-summary.json`), raw);
  const data = JSON.parse(raw).data;
  assert.equal(data.valid, true, name);
  assert.equal(data.measurement.complete, true, name);
  assert.equal(data.tests, expected.passed + expected.skipped, name);
  assert.equal(data.testOutcomes.passed, expected.passed, name);
  assert.equal(data.testOutcomes.skipped, expected.skipped, name);
  assert.equal(data.testOutcomes.failed, 0, name);
  assert.equal(data.testOutcomes.unknown, 0, name);
  console.log(
    `[${name}] ${expected.passed} passing tests; ${expected.skipped} skipped; complete measurement`,
  );
}
run(
  tooling,
  'npm',
  [
    'install',
    '--no-audit',
    '--no-fund',
    '--ignore-scripts',
    'pnpm@11.23.0',
    'yarn@1.22.22',
  ],
  'tooling-install',
);
const bin = join(tooling, 'node_modules/.bin');
env.PATH = `${bin}${process.platform === 'win32' ? ';' : ':'}${env.PATH}`;
const react = clone(
  'bulletproof-react',
  'https://github.com/alan2207/bulletproof-react.git',
  '9506629ed003a561c6627735480cce4994244bb4',
);
const web = join(react, 'apps/react-vite');
run(web, join(bin, 'yarn'), ['install', '--frozen-lockfile'], 'react-install');
copyFileSync(join(web, '.env.example'), join(web, '.env'));
// The pinned discussions test looks for its new row by the minute of a date
// it made before typing the discussion, while the row shows the minute the
// mock server saved it: a run that crosses a minute fails, Supercov or not
// (`react-baseline` on 2026-11-27, 7:05 AM expected, a row of 7:06). The row
// is found by its title and actions with any date between them; the rest of
// the test is unchanged.
const discussions = join(
  web,
  'src/app/routes/app/discussions/__tests__/discussions.test.tsx',
);
const minuteBound =
  'name: `${newDiscussion.title} ${formatDate(newDiscussion.createdAt)} View Delete Discussion`,';
const source = readFileSync(discussions, 'utf8');
assert.equal(source.split(minuteBound).length, 2, discussions);
writeFileSync(
  discussions,
  source.replace(
    minuteBound,
    "name: new RegExp(`^${newDiscussion.title.replace(/[.*+?^${}()|[\\]\\\\]/g, '\\\\$&')} \\\\S.* View Delete Discussion$`),",
  ),
);
// Explicitly scope application code; generators and the mock worker are tooling.
verify(
  web,
  'react',
  'npm',
  ['test', '--', '--run'],
  { passed: 21, skipped: 0 },
  { SUPERCOV_SOURCE_ROOTS: 'src' },
);
const native = clone(
  'bluesky',
  'https://github.com/bluesky-social/social-app.git',
  'a75324b378f13a1a2fb31cd47af7f0f1b4c606bb',
);
run(
  native,
  join(bin, 'pnpm'),
  ['install', '--frozen-lockfile'],
  'bluesky-install',
);
// This pinned checkout omits the generated English catalogue. Keep its one
// dependent test file out of both runs, without inventing translations/mocks.
verify(
  native,
  'bluesky',
  join(bin, 'pnpm'),
  [
    'test',
    '--runInBand',
    '--testPathIgnorePatterns',
    '__tests__/lib/string.test.ts$|node_modules/',
  ],
  { passed: 809, skipped: 28 },
);
console.log(`Full transcripts and summaries: ${audit}`);
