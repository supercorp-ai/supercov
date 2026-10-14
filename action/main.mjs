// Runs a test command under Supercov and says on the pull request what the change left untested.
import { spawnSync } from 'node:child_process';
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { annotation, comment, input, marker, nativeTarget, percentage, ranges, unpack, verify } from './lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const environment = process.env;
const notice = (text) => console.log(`::notice title=Supercov::${text.replaceAll('%', '%25').replaceAll('\n', '%0A')}`);

function libc() {
  if (process.platform !== 'linux') return undefined;
  const musl = existsSync('/etc/alpine-release') || ['/lib', '/usr/lib'].some((directory) => {
    try {
      return readdirSync(directory).some((name) => name.startsWith('ld-musl-') || name.startsWith('libc.musl-'));
    } catch {
      return false;
    }
  });
  return musl ? 'musl' : 'gnu';
}

async function download(url) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      const reply = await fetch(url, { headers: { 'user-agent': 'supercov-action' } });
      if (reply.ok) return Buffer.from(await reply.arrayBuffer());
      if (attempt === 4 || (reply.status < 500 && reply.status !== 429)) throw new Error(`${url}: ${reply.status} ${reply.statusText}`);
    } catch (error) {
      if (attempt === 4 || /: 4\d\d /.test(String(error.message))) throw error;
    }
    await new Promise((done) => setTimeout(done, attempt * 2000));
  }
}

/// The binary for this machine, from the npm registry, kept in the runner's tool cache.
async function install(version) {
  const targets = JSON.parse(readFileSync(join(here, '..', 'npm', 'native-targets.json'), 'utf8')).targets;
  const target = nativeTarget(targets, { platform: process.platform, arch: process.arch, libc: libc() });
  const cache = environment.RUNNER_TOOL_CACHE || environment.RUNNER_TEMP || join(here, '..', '.cache');
  const directory = join(cache, 'supercov', version, process.arch);
  const binary = join(directory, target.executable);
  if (existsSync(binary)) return binary;
  const registry = (environment.NPM_CONFIG_REGISTRY || 'https://registry.npmjs.org').replace(/\/$/, '');
  const listed = async (which) => JSON.parse((await download(`${registry}/${target.package.replace('/', '%2F')}/${which}`)).toString('utf8'));
  let listing;
  try {
    listing = await listed(version);
  } catch (error) {
    if (!/: 404 /.test(error.message)) throw error;
    // A tag is pushed before its packages are published. Until they are, run the newest that is.
    listing = await listed('latest');
    notice(`Supercov ${version} is not published for this machine yet; using ${listing.version}.`);
    return install(listing.version);
  }
  const archive = await download(listing.dist.tarball);
  verify(archive, listing.dist.integrity);
  mkdirSync(directory, { recursive: true });
  // Written beside its place and renamed, so a job sharing the cache never runs half a file.
  const partial = `${binary}.${process.pid}`;
  writeFileSync(partial, unpack(archive, `package/bin/${target.executable}`));
  chmodSync(partial, 0o755);
  renameSync(partial, binary);
  return binary;
}

const git = (cwd, ...arguments_) => spawnSync('git', arguments_, { cwd, encoding: 'utf8' });

/// Fetches history until the commit the change started from is known.
///
/// `actions/checkout` fetches one commit by default, and a change is the difference from where the
/// branch left its base, which that commit alone cannot say.
function reachBase(cwd, base) {
  const known = () => git(cwd, 'merge-base', base, 'HEAD').status === 0;
  if (known()) return true;
  const head = git(cwd, 'rev-parse', 'HEAD').stdout.trim();
  if (git(cwd, 'rev-parse', '--is-shallow-repository').stdout.trim() !== 'true') {
    git(cwd, 'fetch', '--quiet', '--no-tags', 'origin', base);
    return known();
  }
  for (const depth of [1, 10, 50, 200, 1000]) {
    git(cwd, 'fetch', '--quiet', '--no-tags', `--deepen=${depth}`, 'origin', base, head);
    if (known()) return true;
  }
  git(cwd, 'fetch', '--quiet', '--no-tags', '--unshallow', 'origin');
  return known();
}

function query(binary, cwd, arguments_) {
  const result = spawnSync(binary, [...arguments_, '--json'], { cwd, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
  let reply;
  try {
    reply = JSON.parse(result.stdout);
  } catch {
    throw new Error(`supercov ${arguments_.join(' ')}: ${(result.stderr || result.stdout).trim()}`);
  }
  return { status: result.status, data: reply.data, reply };
}

async function github(path, options = {}) {
  const reply = await fetch(`${environment.GITHUB_API_URL || 'https://api.github.com'}${path}`, {
    ...options,
    headers: {
      accept: 'application/vnd.github+json',
      authorization: `Bearer ${input(environment, 'token')}`,
      'content-type': 'application/json',
      'user-agent': 'supercov-action',
      'x-github-api-version': '2022-11-28',
    },
  });
  if (!reply.ok) throw Object.assign(new Error(`${options.method || 'GET'} ${path}: ${reply.status} ${await reply.text()}`), { status: reply.status });
  return reply.json();
}

/// Writes the comment, over the one an earlier run left if there is one.
async function post(repository, number, title, body) {
  const first = marker(title);
  for (let page = 1; page <= 10; page += 1) {
    const comments = await github(`/repos/${repository}/issues/${number}/comments?per_page=100&page=${page}`);
    const mine = comments.find((each) => each.body?.startsWith(first));
    if (mine) {
      await github(`/repos/${repository}/issues/comments/${mine.id}`, { method: 'PATCH', body: JSON.stringify({ body }) });
      return;
    }
    if (comments.length < 100) break;
  }
  await github(`/repos/${repository}/issues/${number}/comments`, { method: 'POST', body: JSON.stringify({ body }) });
}

async function main() {
  const command = input(environment, 'command');
  if (!command) throw new Error('the `command` input is required: the test command to measure, such as `npm test`');
  const workspace = environment.GITHUB_WORKSPACE || process.cwd();
  const cwd = resolve(workspace, input(environment, 'working-directory', '.'));
  const title = input(environment, 'title', 'Coverage');
  const event = environment.GITHUB_EVENT_PATH && existsSync(environment.GITHUB_EVENT_PATH)
    ? JSON.parse(readFileSync(environment.GITHUB_EVENT_PATH, 'utf8'))
    : {};
  const pull = event.pull_request;
  const base = input(environment, 'base', pull?.base?.sha ?? '');

  let binary = input(environment, 'binary');
  let version = input(environment, 'version', JSON.parse(readFileSync(join(here, '..', 'package.json'), 'utf8')).version);
  if (binary) {
    binary = resolve(workspace, binary);
    version = spawnSync(binary, ['--version'], { encoding: 'utf8' }).stdout?.match(/\d+\.\d+\.\d+/)?.[0] ?? version;
  } else {
    binary = await install(version);
    version = spawnSync(binary, ['--version'], { encoding: 'utf8' }).stdout?.match(/\d+\.\d+\.\d+/)?.[0] ?? version;
  }
  if (environment.GITHUB_PATH) appendFileSync(environment.GITHUB_PATH, `${dirname(binary)}\n`);

  // The run this step recorded, by the id it writes: another step of the job
  // may be recording a run in the same checkout at the same time.
  const idFile = join(environment.RUNNER_TEMP || tmpdir(), `supercov-run-id-${process.pid}`);
  console.log(`::group::supercov -- ${command}`);
  const run = spawnSync(`"${binary}" -- ${command}`, {
    cwd,
    shell: true,
    stdio: 'inherit',
    env: { ...environment, SUPERCOV_RUN_ID_FILE: idFile },
  });
  console.log('::endgroup::');
  if (run.error) throw run.error;
  const exit = run.status ?? 1;
  // A Supercov older than 4.7.0 writes no id; its newest run is this one.
  const recorded = existsSync(idFile) ? readFileSync(idFile, 'utf8').trim() || 'latest' : 'latest';

  const outputs = {};
  const floors = [];
  let body;
  if (exit !== 0) {
    body = comment({ title, command, version, failed: exit });
  } else {
    const summary = query(binary, cwd, ['runs', recorded]).data;
    outputs['run-id'] = summary.run;
    outputs.lines = percentage(summary.coverage.lines.covered, summary.coverage.lines.total).replace('%', '');
    outputs.branches = percentage(summary.coverage.branches.covered, summary.coverage.branches.total).replace('%', '');

    const minimums = ['lines', 'branches', 'mcdc'].flatMap((metric) => {
      const floor = input(environment, `min-${metric}`);
      return floor ? [`--min-${metric}`, floor] : [];
    });
    if (minimums.length > 0) {
      const check = query(binary, cwd, ['runs', recorded, 'check', ...minimums]);
      const outcome = check.data?.outcome ?? {};
      for (const violation of outcome.violations ?? []) {
        floors.push(`${violation.metric === 'mcdc' ? 'MC/DC' : violation.metric} ${percentage(violation.covered, violation.eligible)}, below ${violation.floorPpm / 10000}%`);
      }
      if (check.status !== 0 && floors.length === 0) {
        floors.push((outcome.reasons ?? [check.reply?.error?.message ?? 'the run could not be checked']).join('; '));
      }
    }

    let patch;
    let patchError;
    if (base) {
      if (reachBase(cwd, base)) {
        const floor = input(environment, 'min-changed-lines');
        let result = {};
        try {
          result = query(binary, cwd, ['runs', recorded, 'patch', '--base', base, ...(floor ? ['--min-lines', floor] : [])]);
        } catch (error) {
          result = { data: { reasons: [error.message.split('\n')[0]] } };
        }
        if (result.data?.patch) {
          patch = result.data.patch;
          if (patch.eligible > 0) outputs['changed-lines'] = percentage(patch.covered, patch.eligible).replace('%', '');
          if (result.data.result === 'fail') floors.push(`changed lines ${percentage(patch.covered, patch.eligible)}, below ${floor}%`);
        } else {
          patchError = (result.data?.reasons ?? [result.reply?.error?.message ?? 'the run could not be mapped onto the change']).join('; ');
        }
      } else {
        patchError = `the history between this commit and ${base.slice(0, 12)} could not be fetched.`;
      }
    }

    // The run names files from where it ran; a pull request names them from the repository's root.
    const prefix = relative(workspace, cwd).replaceAll('\\', '/');
    const inRepository = (file) => (prefix ? `${prefix}/${file}` : file);
    const sha = pull?.head?.sha ?? environment.GITHUB_SHA;
    const server = environment.GITHUB_SERVER_URL || 'https://github.com';
    const repository = environment.GITHUB_REPOSITORY;
    const links = repository && sha
      ? (file, start, end) => `${server}/${repository}/blob/${sha}/${inRepository(file).split('/').map(encodeURIComponent).join('/')}#L${start}${end === start ? '' : `-L${end}`}`
      : undefined;
    // The runs of servers the tests drove, each its own section.
    const servers = [];
    for (const companion of (summary.companions ?? []).filter((each) => each.role === 'server')) {
      const server = { language: companion.language, summary: query(binary, cwd, ['runs', companion.id]).data };
      if (base && patch) {
        try {
          const result = query(binary, cwd, ['runs', companion.id, 'patch', '--base', base]);
          if (result.data?.patch) server.patch = result.data.patch;
          else server.patchError = (result.data?.reasons ?? ['the run could not be mapped onto the change']).join('; ');
        } catch (error) {
          server.patchError = error.message.split('\n')[0];
        }
      }
      servers.push(server);
    }
    body = comment({ title, command, version, summary, patch, patchError, servers, floors, links });

    if (patch && input(environment, 'annotate', 'true') === 'true') {
      let left = 30;
      for (const file of [patch, ...servers.map((server) => server.patch)].filter(Boolean).flatMap((each) => each.files)) {
        for (const [start, end] of ranges(file.uncovered)) {
          if (left === 0) break;
          left -= 1;
          console.log(annotation(inRepository(file.file), start, end));
        }
      }
    }
  }

  if (environment.GITHUB_STEP_SUMMARY) appendFileSync(environment.GITHUB_STEP_SUMMARY, body.split('\n').slice(1).join('\n'));
  if (environment.GITHUB_OUTPUT) {
    appendFileSync(environment.GITHUB_OUTPUT, Object.entries(outputs).map(([name, value]) => `${name}=${value}\n`).join(''));
  }

  if (pull && input(environment, 'comment', 'true') === 'true') {
    try {
      await post(environment.GITHUB_REPOSITORY, pull.number, title, body);
    } catch (error) {
      // A pull request from a fork runs with a token that may not write. The summary has the same text.
      if (error.status === 403 || error.status === 401 || error.status === 404) {
        notice(`The coverage comment was not posted: the token may not write to pull requests (grant \`pull-requests: write\`; a pull request from a fork never can). The job summary has the same report.`);
      } else {
        throw error;
      }
    }
  }

  if (exit !== 0) process.exit(exit);
  if (floors.length > 0) {
    for (const floor of floors) console.log(`::error title=Coverage below the floor::${floor}`);
    process.exit(1);
  }
}

main().catch((error) => {
  console.log(`::error title=Supercov::${String(error.message).replaceAll('%', '%25').replaceAll('\n', '%0A')}`);
  process.exit(2);
});
