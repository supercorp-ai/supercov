// scripts/wait-ci.sh on a pull request: the checks its base requires decide,
// and one not posted yet is waited for, not passed. Run against a stand-in
// `gh` that answers each poll from a list, and a `sleep` that returns at once.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';

const script = resolve(import.meta.dirname, '../../scripts/wait-ci.sh');

const status = (context, state) => ({ context, state, isRequired: true });
const check = (name, conclusion) => ({ name, status: conclusion ? 'COMPLETED' : 'IN_PROGRESS', conclusion, isRequired: false });

/** One poll's answer: the head commit's checks, counted as GitHub counts them. */
function poll(nodes) {
  const count = (list, states) => states.map((state) => ({ state, count: list.filter((node) => (node.state ?? node.conclusion ?? node.status) === state).length }));
  const statuses = nodes.filter((node) => node.context);
  const checks = nodes.filter((node) => !node.context);
  return {
    data: {
      rateLimit: { remaining: 5000, resetAt: '2026-01-01T00:00:00Z' },
      repository: { pullRequest: { state: 'OPEN', commits: { nodes: [{ commit: { oid: 'abcdef0123456789', statusCheckRollup: { state: 'PENDING', contexts: {
        totalCount: nodes.length,
        checkRunCountsByState: count(checks, ['SUCCESS', 'FAILURE', 'IN_PROGRESS']),
        statusContextCountsByState: count(statuses, ['SUCCESS', 'FAILURE', 'PENDING']),
        pageInfo: { hasNextPage: false, endCursor: null },
        nodes,
      } } } }] } } },
    },
  };
}

/** Runs the script against answers given poll by poll (the last repeats). */
function wait(required, polls) {
  const directory = mkdtempSync(join(tmpdir(), 'wait-ci-'));
  try {
    const rules = { data: { repository: { pullRequest: { baseRef: {
      rules: { nodes: [{ parameters: { requiredStatusChecks: required.map((context) => ({ context })) } }] },
      branchProtectionRule: null,
    } } } } };
    writeFileSync(join(directory, 'rules.json'), JSON.stringify(rules));
    polls.forEach((nodes, index) => writeFileSync(join(directory, `poll-${index + 1}.json`), JSON.stringify(poll(nodes))));
    writeFileSync(join(directory, 'gh'), `#!/bin/sh
case "$*" in
  *baseRef*) cat "${directory}/rules.json"; exit 0;;
esac
n=$(cat "${directory}/count" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "${directory}/count"
[ -f "${directory}/poll-$n.json" ] || n=${polls.length}
cat "${directory}/poll-$n.json"
`);
    writeFileSync(join(directory, 'sleep'), '#!/bin/sh\nexit 0\n');
    chmodSync(join(directory, 'gh'), 0o755);
    chmodSync(join(directory, 'sleep'), 0o755);
    const run = spawnSync('bash', [script, 'https://github.com/owner/repo/pull/7'], {
      encoding: 'utf8',
      env: { ...process.env, PATH: `${directory}:${process.env.PATH}` },
      timeout: 30_000,
    });
    return { code: run.status, out: run.stdout, polls: Number(spawnSync('cat', [join(directory, 'count')], { encoding: 'utf8' }).stdout.trim()) };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('a required status not posted yet is waited for, then decides', () => {
  const deferred = status('Slow tier', 'SUCCESS');
  const passed = wait(['Fast tier', 'Slow tier'], [[deferred, check('lint', null)], [deferred, check('lint', 'SUCCESS'), status('Fast tier', 'SUCCESS')]]);
  assert.equal(passed.code, 0, passed.out);
  assert.equal(passed.polls, 2, passed.out);
  assert.match(passed.out, /not posted yet: Fast tier/u);
  assert.match(passed.out, /finished: required checks passed/u);
  const failed = wait(['Fast tier', 'Slow tier'], [[deferred], [deferred, status('Fast tier', 'FAILURE')]]);
  assert.equal(failed.code, 1, failed.out);
  assert.match(failed.out, /failed: Fast tier/u);
});

test('with every required status there, and none required, it decides on the first poll', () => {
  const both = wait(['Fast tier', 'Slow tier'], [[status('Slow tier', 'SUCCESS'), status('Fast tier', 'SUCCESS')]]);
  assert.deepEqual([both.code, both.polls], [0, 1], both.out);
  const none = wait([], [[check('lint', 'SUCCESS')]]);
  assert.deepEqual([none.code, none.polls], [0, 1], none.out);
});
