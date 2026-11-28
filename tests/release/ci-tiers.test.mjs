// The two tiers: when the slow tier runs, the status each tier posts, and
// what a release requires of the statuses of its tree.
import assert from 'node:assert/strict';
import test from 'node:test';
import { decide, IN_FULL, IN_QUEUE, missing, queuedNumber, verdict, withQueuedPull } from '../../scripts/ci-tiers.mjs';

const pull = (fields = {}) => ({ title: 'Measure the thing', head: { ref: 'feature/x', sha: 'abc' }, labels: [], draft: false, ...fields });

test('a pull request leaves the slow tier to the merge queue, and says so as a passing status', () => {
  for (const action of ['opened', 'synchronize', 'reopened', 'ready_for_review']) {
    const decided = decide('pull_request', { action, pull_request: pull({ labels: [{ name: 'ready' }] }) });
    assert.deepEqual([decided.run, decided.deferred, decided.pending], [false, IN_QUEUE, undefined]);
  }
  // A draft cannot be queued, and says the same.
  assert.equal(decide('pull_request', { action: 'synchronize', pull_request: pull({ draft: true }) }).deferred, IN_QUEUE);
  // It is not "in full": a release reads the statuses of the commit the queue merged.
  assert.equal(IN_QUEUE.startsWith(IN_FULL), false);
  // The ready label, or any label but full-ci, asks for nothing and leaves the commit as it is.
  for (const name of ['ready', 'documentation']) {
    const labelled = decide('pull_request', { action: 'labeled', label: { name }, pull_request: pull({ labels: [{ name }] }) });
    assert.deepEqual([labelled.run, labelled.quiet, labelled.deferred, labelled.pending], [false, true, undefined, undefined]);
  }
});

test('full-ci and a release pull request run it on the pull request as well, in full; main each night', () => {
  const everything = decide('pull_request', { action: 'labeled', label: { name: 'full-ci' }, pull_request: pull({ labels: [{ name: 'full-ci' }] }) });
  assert.deepEqual([everything.run, everything.full], [true, true]);
  // A later push to a full-ci pull request leaves it to the queue, which runs it in full.
  assert.equal(decide('pull_request', { action: 'synchronize', pull_request: pull({ labels: [{ name: 'full-ci' }] }) }).deferred, IN_QUEUE);
  for (const fields of [{ title: 'Release 4.30.0' }, { head: { ref: 'release/4.30.0', sha: 'abc' } }]) {
    const release = decide('pull_request', { action: 'synchronize', pull_request: pull(fields) });
    assert.deepEqual([release.run, release.full], [true, true]);
  }
  const draft = decide('pull_request', { action: 'labeled', label: { name: 'full-ci' }, pull_request: pull({ draft: true, labels: [{ name: 'full-ci' }] }) });
  assert.equal(draft.run, false);
  assert.match(draft.pending, /draft/u);
  assert.deepEqual([decide('schedule').run, decide('schedule').full], [true, true]);
  assert.deepEqual([decide('workflow_dispatch').run, decide('workflow_dispatch').full], [true, true]);
  assert.equal(decide('push').run, false);
});

test('a tier passes when no job failed or was cancelled, and says whether every test ran', () => {
  assert.deepEqual(verdict({ plan: { result: 'success' }, check: { result: 'success' }, wasm: { result: 'skipped' } }, true), { state: 'success', description: IN_FULL });
  assert.equal(verdict({ plan: { result: 'success' } }, false).description.startsWith(IN_FULL), false);
  assert.deepEqual(verdict({ plan: { result: 'success' }, check: { result: 'failure' }, lint: { result: 'cancelled' } }, true),
    { state: 'failure', description: 'check failed, lint was cancelled' });
  assert.equal(verdict({}, true).state, 'failure');
});

test('a release needs both tiers in full, from GitHub Actions, as their newest statuses say', () => {
  const by = (context, state, description, login = 'github-actions[bot]') => ({ context, state, description, creator: { login } });
  assert.deepEqual(missing([by('Fast tier', 'success', IN_FULL), by('Slow tier', 'success', IN_FULL)]), []);
  // Newest first: a later failure stands over an earlier pass.
  assert.deepEqual(missing([by('Slow tier', 'failure', 'x failed'), by('Fast tier', 'success', IN_FULL), by('Slow tier', 'success', IN_FULL)]), ['Slow tier is failure']);
  assert.deepEqual(missing([by('Fast tier', 'success', 'Passed on what the change can affect'), by('Slow tier', 'pending', 'Running')]),
    ['Fast tier passed on part of the tests only', 'Slow tier is pending']);
  // Anyone who can push can post a status: only GitHub Actions' count.
  assert.deepEqual(missing([by('Fast tier', 'success', IN_FULL, 'someone'), by('Slow tier', 'success', IN_FULL, 'someone')]), ['no Fast tier status', 'no Slow tier status']);
});

test('the newest run that set a tier on a commit stands, and an older one does not set it again', async () => {
  const { standingRun } = await import('../../scripts/ci-tiers.mjs');
  const by = (context, run, login = 'github-actions[bot]') => ({ context, creator: { login }, target_url: `https://github.com/o/r/actions/runs/${run}` });
  assert.equal(standingRun([by('Slow tier', 45833849393), by('Slow tier', 45833800101)], 'Slow tier'), 45833849393);
  assert.equal(standingRun([by('Fast tier', 7)], 'Slow tier'), 0);
  assert.equal(standingRun([by('Slow tier', 9, 'someone')], 'Slow tier'), 0);
  assert.equal(standingRun([], 'Slow tier'), 0);
});

test("the merge queue's commit runs the slow tier as its pull request would", () => {
  const group = (fields) => ({
    action: 'checks_requested',
    merge_group: { head_ref: 'refs/heads/gh-readonly-queue/main/pr-355-0123456789abcdef0123456789abcdef01234567', base_sha: 'b', head_sha: 'h' },
    ...(fields ? { pull_request: pull(fields) } : {}),
  });
  assert.equal(queuedNumber(group()), 355);
  assert.equal(queuedNumber({ merge_group: { head_ref: 'refs/heads/main' } }), 0);
  assert.equal(queuedNumber({}), 0);
  // The group's pull request is read for its title and labels; other events are left as they are.
  const read = withQueuedPull('merge_group', group(), (number) => pull({ number }));
  assert.equal(read.pull_request.number, 355);
  const untouched = { action: 'opened', pull_request: pull() };
  assert.equal(withQueuedPull('pull_request', untouched, () => assert.fail('read a pull request')), untouched);
  assert.equal(withQueuedPull('merge_group', { merge_group: { head_ref: 'refs/heads/x' } }, () => assert.fail('read a pull request')).pull_request, undefined);
  // On the changed lines, or in full for a release pull request or full-ci: the label that ran it on the pull request is not needed again.
  const plain = decide('merge_group', group({ labels: [{ name: 'ready' }] }));
  assert.deepEqual([plain.run, plain.full, plain.pending, plain.stale], [true, false, undefined, undefined]);
  assert.match(plain.reason, /#355, on the changed lines/u);
  assert.deepEqual([decide('merge_group', group({})).run, decide('merge_group', group({})).full], [true, false]);
  assert.equal(decide('merge_group', group({ title: 'Release 4.30.0' })).full, true);
  assert.equal(decide('merge_group', group({ labels: [{ name: 'full-ci' }] })).full, true);
  assert.deepEqual([decide('merge_group', { merge_group: {} }).run, decide('merge_group', { merge_group: {} }).full], [true, false]);
});
