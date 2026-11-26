// The two tiers: when the slow tier runs, the status each tier posts, and
// what a release requires of the statuses of its tree.
import assert from 'node:assert/strict';
import test from 'node:test';
import { decide, IN_FULL, missing, verdict } from '../../scripts/ci-tiers.mjs';

const pull = (fields = {}) => ({ title: 'Measure the thing', head: { ref: 'feature/x', sha: 'abc' }, labels: [], draft: false, ...fields });

test('a push to a pull request runs the fast tier alone, and outlives its ready label', () => {
  const pushed = decide('pull_request', { action: 'synchronize', pull_request: pull({ labels: [{ name: 'ready' }] }) });
  assert.equal(pushed.run, false);
  assert.equal(pushed.stale, true);
  assert.match(pushed.pending, /add the `ready` label/u);
  const opened = decide('pull_request', { action: 'opened', pull_request: pull() });
  assert.equal(opened.run, false);
  assert.equal(opened.stale, false);
});

test('the ready label, ready for review and full-ci run it; another label leaves the commit as it is', () => {
  const ready = decide('pull_request', { action: 'labeled', label: { name: 'ready' }, pull_request: pull({ labels: [{ name: 'ready' }] }) });
  assert.deepEqual([ready.run, ready.full], [true, false]);
  assert.equal(decide('pull_request', { action: 'ready_for_review', pull_request: pull() }).run, true);
  assert.equal(decide('pull_request', { action: 'opened', pull_request: pull({ labels: [{ name: 'ready' }] }) }).run, true);
  const everything = decide('pull_request', { action: 'labeled', label: { name: 'full-ci' }, pull_request: pull({ labels: [{ name: 'full-ci' }] }) });
  assert.deepEqual([everything.run, everything.full], [true, true]);
  const other = decide('pull_request', { action: 'labeled', label: { name: 'documentation' }, pull_request: pull() });
  assert.deepEqual([other.run, other.quiet, other.pending], [false, true, undefined]);
});

test('a draft never runs it, a release pull request runs it in full on every push, and main each night', () => {
  const draft = decide('pull_request', { action: 'labeled', label: { name: 'ready' }, pull_request: pull({ draft: true }) });
  assert.equal(draft.run, false);
  assert.match(draft.pending, /draft/u);
  for (const fields of [{ title: 'Release 4.30.0' }, { head: { ref: 'release/4.30.0', sha: 'abc' } }]) {
    const release = decide('pull_request', { action: 'synchronize', pull_request: pull(fields) });
    assert.deepEqual([release.run, release.full], [true, true]);
  }
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
