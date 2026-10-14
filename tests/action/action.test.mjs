// The action, run as the runner runs it: inputs in the environment, a pull request's event in a
// file, and a local server where the GitHub API would be.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { annotation, comment, input, marker, nativeTarget, percentage, ranges, unpack, verify } from '../../action/lib.mjs';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const targets = JSON.parse(readFileSync(join(repository, 'npm/native-targets.json'), 'utf8')).targets;

function entry(name, content) {
  const header = Buffer.alloc(512);
  header.write(name, 0);
  header.write(content.length.toString(8).padStart(11, '0'), 124);
  header.write('0', 156);
  const padded = Buffer.alloc(Math.ceil(content.length / 512) * 512);
  content.copy(padded);
  return Buffer.concat([header, padded]);
}

test('each machine a release is built for has one native package', () => {
  assert.equal(nativeTarget(targets, { platform: 'linux', arch: 'x64', libc: 'gnu' }).package, '@supercov/cli-linux-x64-gnu');
  assert.equal(nativeTarget(targets, { platform: 'linux', arch: 'arm64', libc: 'musl' }).package, '@supercov/cli-linux-arm64-musl');
  assert.equal(nativeTarget(targets, { platform: 'darwin', arch: 'arm64' }).package, '@supercov/cli-darwin-arm64');
  assert.equal(nativeTarget(targets, { platform: 'win32', arch: 'x64' }).executable, 'supercov.exe');
  assert.throws(() => nativeTarget(targets, { platform: 'freebsd', arch: 'x64' }), /no Supercov release is built for freebsd\/x64/);
});

test('the binary is read out of the archive, and an archive that does not match its digest is refused', () => {
  const body = Buffer.from('#!/bin/sh\necho supercov\n'.repeat(60));
  const archive = gzipSync(Buffer.concat([entry('package/LICENSE', Buffer.from('MIT')), entry('package/bin/supercov', body), Buffer.alloc(1024)]));
  assert.deepEqual(unpack(archive, 'package/bin/supercov'), body);
  assert.throws(() => unpack(archive, 'package/bin/supercov.exe'), /holds no package\/bin\/supercov.exe/);
  const integrity = `sha512-${createHash('sha512').update(archive).digest('base64')}`;
  verify(archive, integrity);
  assert.throws(() => verify(Buffer.concat([archive, Buffer.from('x')]), integrity), /does not match its sha512 digest/);
});

test('lines are grouped, and a percentage is never rounded up', () => {
  assert.deepEqual(ranges([3, 4, 5, 9, 11, 12]), [[3, 5], [9, 9], [11, 12]]);
  assert.equal(percentage(9999, 10000), '99.99%');
  assert.equal(percentage(199999, 200000), '99.99%');
  assert.equal(percentage(2, 3), '66.66%');
  assert.equal(percentage(0, 0), 'n/a');
});

test('inputs are read as the runner names them', () => {
  assert.equal(input({ 'INPUT_WORKING-DIRECTORY': ' web ' }, 'working-directory', '.'), 'web');
  assert.equal(input({ 'INPUT_WORKING-DIRECTORY': '' }, 'working-directory', '.'), '.');
});

test('a failing command is said, with no coverage beside it', () => {
  const body = comment({ title: 'Coverage', command: 'npm test', version: '4.4.0', failed: 1 });
  assert.ok(body.startsWith(marker('Coverage')));
  assert.match(body, /The test command `npm test` failed \(exit 1\)/);
  assert.doesNotMatch(body, /\| Lines \|/);
});

test('a server the tests drove has its own section, and a file it measured is not missing', () => {
  const coverage = (covered, total) => ({ lines: { covered, total }, branches: { covered: 1, total: 2 }, coveredConditions: 0, conditions: 0 });
  const body = comment({
    title: 'Coverage',
    command: 'npx playwright test',
    version: '4.9.0',
    summary: { coverage: coverage(7, 7), tests: 5 },
    patch: { eligible: 2, covered: 2, files: [{ file: 'src/front.js', uncovered: [] }], missingFromRun: ['app/views.py', 'docs/notes.py'] },
    servers: [
      {
        language: 'Python',
        summary: { coverage: coverage(28, 30), tests: 5 },
        patch: { eligible: 4, covered: 3, files: [{ file: 'app/views.py', uncovered: [12] }], missingFromRun: ['src/front.js', 'docs/notes.py'] },
      },
    ],
  });
  assert.match(body, /#### Python server\n\n\*\*Changed lines: 75\.00% \(3 of 4\)\*\* covered by a test\./);
  assert.match(body, /\| `app\/views\.py` \| 12 \|/);
  // Missing only where no run measured it.
  assert.match(body, /not run by the test command at all: `docs\/notes\.py`\n/);
  assert.doesNotMatch(body, /at all: .*views\.py/);
});

test('an annotation escapes what would end the command', () => {
  assert.equal(
    annotation('src/a,b:c.js', 4, 6),
    '::warning file=src/a%2Cb%3Ac.js,line=4,endLine=6,title=Not covered::Changed lines 4 to 6 not run by any test.',
  );
});

const git = (cwd, ...arguments_) => execFileSync('git', arguments_, { cwd, encoding: 'utf8', env: { ...process.env, GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com' } }).trim();

/// A project with a tested function, and a change that adds one half of which no test runs.
function project() {
  const root = realpathSync.native(mkdtempSync(join(tmpdir(), 'supercov-action-')));
  const web = join(root, 'web');
  mkdirSync(join(web, 'src'), { recursive: true });
  mkdirSync(join(web, 'test'), { recursive: true });
  writeFileSync(join(web, 'package.json'), JSON.stringify({ name: 'web', type: 'module', private: true }));
  writeFileSync(join(web, 'src/price.js'), 'export function total(price, quantity) {\n  return price * quantity;\n}\n');
  writeFileSync(join(web, 'test/price.test.js'), "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { total } from '../src/price.js';\n\ntest('total', () => {\n  assert.equal(total(2, 3), 6);\n});\n");
  writeFileSync(join(root, '.gitignore'), '.supercov\n');
  git(root, 'init', '-q', '-b', 'main');
  git(root, 'add', '-A');
  git(root, 'commit', '-q', '-m', 'A price');
  const base = git(root, 'rev-parse', 'HEAD');
  writeFileSync(
    join(web, 'src/price.js'),
    'export function total(price, quantity) {\n  return price * quantity;\n}\n\nexport function discounted(price, code) {\n  if (code === "half") {\n    return price / 2;\n  }\n  return price;\n}\n',
  );
  writeFileSync(
    join(web, 'test/price.test.js'),
    "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { discounted, total } from '../src/price.js';\n\ntest('total', () => {\n  assert.equal(total(2, 3), 6);\n});\n\ntest('no code', () => {\n  assert.equal(discounted(10, ''), 10);\n});\n",
  );
  git(root, 'add', '-A');
  git(root, 'commit', '-q', '-m', 'A discount');
  return { root, base, head: git(root, 'rev-parse', 'HEAD') };
}

/// Stands where the GitHub API would, keeping the comments it is given.
async function api(status = 200) {
  const comments = [];
  const calls = [];
  const server = http.createServer((request, reply) => {
    const chunks = [];
    request.on('data', (chunk) => chunks.push(chunk));
    request.on('end', () => {
      const body = chunks.length > 0 ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : undefined;
      calls.push({ method: request.method, url: request.url, authorization: request.headers.authorization });
      reply.setHeader('content-type', 'application/json');
      if (status !== 200) {
        reply.writeHead(status);
        reply.end('{"message":"Resource not accessible by integration"}');
      } else if (request.method === 'GET') {
        reply.end(JSON.stringify(comments));
      } else if (request.method === 'POST') {
        comments.push({ id: comments.length + 1, body: body.body });
        reply.end(JSON.stringify(comments.at(-1)));
      } else {
        const id = Number(request.url.split('/').at(-1));
        comments.find((each) => each.id === id).body = body.body;
        reply.end('{}');
      }
    });
  });
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
  return { comments, calls, url: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}

function act({ root, base, head }, url, inputs = {}) {
  const files = mkdtempSync(join(tmpdir(), 'supercov-action-files-'));
  const event = join(files, 'event.json');
  writeFileSync(event, JSON.stringify({ pull_request: { number: 7, base: { sha: base }, head: { sha: head } } }));
  const environment = {
    ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('INPUT_') && !name.startsWith('GITHUB_') && name !== 'NODE_TEST_CONTEXT')),
    GITHUB_WORKSPACE: root,
    GITHUB_EVENT_PATH: event,
    GITHUB_REPOSITORY: 'acme/shop',
    GITHUB_API_URL: url,
    GITHUB_STEP_SUMMARY: join(files, 'summary.md'),
    GITHUB_OUTPUT: join(files, 'output'),
    GITHUB_PATH: join(files, 'path'),
    INPUT_COMMAND: 'node --test',
    'INPUT_WORKING-DIRECTORY': 'web',
    INPUT_BINARY: binary,
    INPUT_TOKEN: 'a-token',
    INPUT_COMMENT: 'true',
    INPUT_ANNOTATE: 'true',
    INPUT_TITLE: 'Coverage',
    ...inputs,
  };
  return new Promise((done) => {
    const child = spawn(process.execPath, [join(repository, 'action/main.mjs')], { cwd: root, env: environment });
    let output = '';
    child.stdout.on('data', (chunk) => (output += chunk));
    child.stderr.on('data', (chunk) => (output += chunk));
    child.on('close', (status) => {
      const read = (name) => (existsSync(join(files, name)) ? readFileSync(join(files, name), 'utf8') : '');
      done({ status, output, summary: read('summary.md'), outputs: read('output'), path: read('path') });
    });
  });
}

test('a pull request is told which changed lines no test ran, in one comment kept up to date', { skip: !existsSync(binary) && `no binary at ${binary}` }, async () => {
  const change = project();
  const github = await api();
  try {
    const first = await act(change, github.url);
    assert.equal(first.status, 0, first.output);
    assert.equal(github.comments.length, 1);
    const body = github.comments[0].body;
    assert.ok(body.startsWith('<!-- supercov-coverage-comment: Coverage -->\n### Coverage\n'), body);
    assert.match(body, /\*\*Changed lines: 75\.00% \(3 of 4\)\*\* covered by a test\./);
    assert.match(body, /\| Lines \| Branches \| MC\/DC \| Tests \|\n\| --- \| --- \| --- \| --- \|\n\| 83\.33% \(5 of 6\) \| 50\.00% \(1 of 2\) \| [^|]+ \| 2 \|/);
    // The file is named from where the tests ran, and linked from the repository's root at the pull request's commit.
    assert.ok(body.includes(`| \`src/price.js\` | [7](https://github.com/acme/shop/blob/${change.head}/web/src/price.js#L7) |`), body);
    assert.match(body, /Measured by <a href="https:\/\/supercov.com">Supercov<\/a> \d+\.\d+\.\d+ running `node --test`\./);
    assert.ok(first.output.includes('::warning file=web/src/price.js,line=7,endLine=7,title=Not covered::Changed line 7 not run by any test.'), first.output);
    assert.ok(first.summary.startsWith('### Coverage\n'), first.summary);
    assert.match(first.outputs, /^run-id=run_\w+\nlines=83\.33\nbranches=50\.00\nchanged-lines=75\.00\n$/);
    assert.equal(first.path.trim(), dirname(binary));
    assert.ok(github.calls.every((call) => call.authorization === 'Bearer a-token'));

    // A second push rewrites that comment and adds none.
    const second = await act(change, github.url);
    assert.equal(second.status, 0, second.output);
    assert.equal(github.comments.length, 1);
    assert.deepEqual(github.calls.map((call) => call.method), ['GET', 'POST', 'GET', 'PATCH']);

    // Another job's comment, under its own title, stands beside it.
    await act(change, github.url, { INPUT_TITLE: 'Coverage (web)' });
    assert.equal(github.comments.length, 2);
  } finally {
    github.close();
  }
});

test('a floor the change misses fails the step and is said in the comment', { skip: !existsSync(binary) && `no binary at ${binary}` }, async () => {
  const change = project();
  const github = await api();
  try {
    const result = await act(change, github.url, { 'INPUT_MIN-CHANGED-LINES': '80', 'INPUT_MIN-BRANCHES': '90' });
    assert.equal(result.status, 1, result.output);
    const body = github.comments[0].body;
    assert.match(body, /\*\*Below the floor:\*\* branches 50\.00%, below 90%/);
    assert.match(body, /\*\*Below the floor:\*\* changed lines 75\.00%, below 80%/);
    assert.match(result.output, /::error title=Coverage below the floor::changed lines 75\.00%, below 80%/);
  } finally {
    github.close();
  }
});

test('a failing test command fails the step with its own status, and the comment says so', { skip: !existsSync(binary) && `no binary at ${binary}` }, async () => {
  const change = project();
  writeFileSync(join(change.root, 'web/test/broken.test.js'), "import test from 'node:test';\ntest('broken', () => { throw new Error('no'); });\n");
  const github = await api();
  try {
    const result = await act(change, github.url);
    assert.equal(result.status, 1, result.output);
    assert.match(github.comments[0].body, /The test command `node --test` failed \(exit 1\), so there is no coverage to report/);
    assert.equal(result.outputs, '');
  } finally {
    github.close();
  }
});

test('a token that may not comment leaves the summary and does not fail the step', { skip: !existsSync(binary) && `no binary at ${binary}` }, async () => {
  const change = project();
  const github = await api(403);
  try {
    const result = await act(change, github.url);
    assert.equal(result.status, 0, result.output);
    assert.match(result.output, /::notice title=Supercov::The coverage comment was not posted/);
    assert.match(result.summary, /\*\*Changed lines: 75\.00% \(3 of 4\)\*\*/);
  } finally {
    github.close();
  }
});
