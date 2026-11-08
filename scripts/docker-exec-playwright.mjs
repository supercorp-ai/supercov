// A suite whose tests run in a container is attributed test by test.
//
//   node scripts/docker-exec-playwright.mjs
//
// The command starts a `node` container with the project mounted at
// /workspace and runs Playwright in it with `docker exec`. Supercov runs the
// command in its copy, so the mount is of the copy; the relay gives the exec
// the test command's settings, each path into the copy moved to /workspace,
// and Playwright runs under the preload and the generated configuration as
// it would beside Supercov. Without them each test's coverage was no test's.
// A command there that sends nothing back is said to have.
// Without Docker there is nothing to run, and the gate says so.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, 'target/debug/supercov');
const root = resolve(repository, 'target', `docker-exec-playwright-${process.pid}-${Date.now()}`);
const PLAYWRIGHT = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8')).devDependencies['@playwright/test'];
const IMAGE = `node:${process.versions.node.split('.')[0]}-bookworm-slim`;
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

const docker = spawnSync('docker', ['info', '--format', '{{.ServerVersion}}'], { encoding: 'utf8' });
if (process.platform === 'win32' || docker.status !== 0) {
  console.log('[docker-exec-playwright] skipped: no Docker daemon here');
  process.exit(0);
}

const name = `supercov-docker-exec-${process.pid}`;
// The container runs as the user who owns the checkout, as a CI job's
// own containers are told to, so what it writes in the copy is theirs.
const user = `${process.getuid()}:${process.getgid()}`;
const files = {
  'package.json': `{ "name": "docker-exec", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}" } }\n`,
  'playwright.config.js': "export default { testDir: 'tests', workers: 1, reporter: 'line' };\n",
  'src/grade.js': "export function grade(score) {\n  if (score >= 90) {\n    return 'A';\n  }\n  return 'fail';\n}\n",
  'tests/grade.spec.js': `import { expect, test } from '@playwright/test';
import { grade } from '../src/grade.js';

test('a high score is an A', () => {
  expect(grade(95)).toBe('A');
});

test('a low score fails', () => {
  expect(grade(10)).toBe('fail');
});
`,
  // A short container of the same image first, as an install is: Node runs
  // there and loads nothing measured, so nothing is missing from it.
  'run-in-container.sh': `set -e
docker run --rm --user ${user} -e HOME=/tmp -v "$PWD":/workspace -w /workspace ${IMAGE} node -e "require('node:fs').readdirSync('.')"
docker run -d --name ${name} --user ${user} -e HOME=/tmp -v "$PWD":/workspace -w /workspace ${IMAGE} sleep infinity >/dev/null
status=0
docker exec ${name} node_modules/.bin/playwright test || status=$?
docker rm -f ${name} >/dev/null
exit $status
`,
  // The same, with the container's own user, root, as sealed workers run:
  // what it writes -- the evidence, Playwright's test-results -- the run, as
  // the checkout's user, still reads and removes.
  'run-as-root.sh': `set -e
docker run -d --name ${name} -v "$PWD":/workspace -w /workspace ${IMAGE} sleep infinity >/dev/null
status=0
docker exec ${name} node_modules/.bin/playwright test || status=$?
docker rm -f ${name} >/dev/null
exit $status
`,
  // The project mounted read-only, as a sealed worker mounts it: the code
  // runs and records into Supercov's own directory, which stays writable.
  'run-read-only.sh': `set -e
docker run -d --name ${name} --user ${user} -e HOME=/tmp -v "$PWD":/workspace:ro -w /workspace ${IMAGE} sleep infinity >/dev/null
status=0
docker exec ${name} node -e "import('./src/grade.js').then((m) => { if (m.grade(95) !== 'A') process.exit(3); })" || status=$?
docker rm -f ${name} >/dev/null
exit $status
`,
  // Tests on the host, beside a container given the settings that records
  // nothing -- a server that stopped at its first probe: the run says so,
  // whatever the tests did.
  'run-silent-server.sh': `set -e
docker run -d --name ${name} --user ${user} -v "$PWD":/workspace -w /workspace ${IMAGE} sleep infinity >/dev/null
status=0
node_modules/.bin/playwright test || status=$?
docker rm -f ${name} >/dev/null
exit $status
`,
  // The same container, given the settings, and a command in it that sends
  // nothing back: the run says so rather than reading as a run of nothing.
  'run-nothing.sh': `set -e
docker run -d --name ${name} --user ${user} -v "$PWD":/workspace -w /workspace ${IMAGE} sleep infinity >/dev/null
docker exec ${name} true
docker rm -f ${name} >/dev/null
`,
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const pulled = run(root, 'docker', ['pull', '-q', IMAGE]);
  assert.equal(pulled.status, 0, pulled.stderr);
  const installed = run(root, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(run(root, 'git', ['init', '-q']).status, 0);
  const measured = run(root, binary, ['--', 'sh', 'run-in-container.sh']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  assert.doesNotMatch(measured.stderr, /came back from/u, measured.stderr);
  const id = JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs[0].id;
  const lines = (test) => run(root, binary, ['runs', id, 'test', test]).stdout.match(/src\/grade\.js:\d+/gu) ?? [];
  const high = lines('a high score is an A');
  const low = lines('a low score fails');
  assert.ok(high.includes('src/grade.js:3') && !high.includes('src/grade.js:5'), `${high}\n${measured.stderr}`);
  assert.ok(low.includes('src/grade.js:5') && !low.includes('src/grade.js:3'), `${low}\n${measured.stderr}`);
  const rooted = run(root, binary, ['--', 'sh', 'run-as-root.sh']);
  assert.equal(rooted.status, 0, `${rooted.stdout}${rooted.stderr}`);
  const rootedId = JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs[0].id;
  assert.notEqual(rootedId, id, `no run was recorded\n${rooted.stderr}`);
  const rootedLines = run(root, binary, ['runs', rootedId, 'test', 'a high score is an A']).stdout;
  assert.match(rootedLines, /src\/grade\.js:3/u, `${rootedLines}\n${rooted.stderr}`);
  const sealed = run(root, binary, ['--', 'sh', 'run-read-only.sh']);
  assert.equal(sealed.status, 0, `${sealed.stdout}${sealed.stderr}`);
  assert.doesNotMatch(`${sealed.stdout}${sealed.stderr}`, /EROFS|read-only file system/u);
  const summary = run(root, binary, ['runs', 'latest']).stdout;
  assert.match(summary, /Lines +[\d.]+% \([1-9]\d*\//u, `${summary}\n${sealed.stderr}`);
  const silent = run(root, binary, ['--', 'sh', 'run-silent-server.sh']);
  assert.equal(silent.status, 0, `${silent.stdout}${silent.stderr}`);
  assert.match(silent.stderr, /warning: no coverage came back from node:\d+-bookworm-slim, which ran/u, silent.stderr);
  assert.doesNotMatch(silent.stderr, /no test came back/u, silent.stderr);
  const nothing = run(root, binary, ['--', 'sh', 'run-nothing.sh']);
  assert.equal(nothing.status, 0, `${nothing.stdout}${nothing.stderr}`);
  assert.match(nothing.stderr, /warning: no test came back from `docker exec` in supercov-docker-exec-\d+, which ran/u, nothing.stderr);
  // The container the tests ran in served nothing: no word of its coverage.
  assert.doesNotMatch(nothing.stderr, /no coverage came back/u, nothing.stderr);
  console.log('[docker-exec-playwright] Playwright run by `docker exec` in a container the copy is mounted into credits each test its own lines');
} finally {
  spawnSync('docker', ['rm', '-f', name], { stdio: 'ignore' });
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'docker-exec-playwright-')))
    rmSync(root, { recursive: true, force: true });
}
