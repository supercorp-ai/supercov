// The merge queue sweep cancels exactly the runs of commits the queue left:
// never a live group's, this event's, or any run that is not a merge group's.
import assert from 'node:assert/strict';
import test from 'node:test';
import { superseded } from '../../scripts/queue-sweep.mjs';

const run = (id, sha, fields = {}) => ({ id, name: 'CI', event: 'merge_group', status: 'queued', head_sha: sha, head_branch: `gh-readonly-queue/main/pr-${id}-base`, ...fields });

test("a merge group's run is cancelled once the queue left its commit, and only then", () => {
  const runs = [
    run(1, 'old-group'),
    run(2, 'live-group'),
    run(3, 'old-group', { name: 'Slow tier', status: 'in_progress' }),
    run(4, 'this-event'),
    run(5, 'waiting-old', { status: 'waiting' }),
  ];
  assert.deepEqual(superseded(runs, ['live-group', 'this-event']).map((each) => each.id), [1, 3, 5]);
  // Nothing in the queue but this event's group: every other merge-group run goes.
  assert.deepEqual(superseded(runs, ['this-event', '', null]).map((each) => each.id), [1, 2, 3, 5]);
});

test('never a run that is not a merge group on a queue branch, nor a finished one', () => {
  const others = [
    run(10, 'x', { event: 'push', head_branch: 'main' }),
    run(11, 'x', { event: 'pull_request', head_branch: 'feature' }),
    run(12, 'x', { event: 'schedule', head_branch: 'main' }),
    run(13, 'x', { head_branch: 'release/4.30.0' }),
    run(14, 'x', { status: 'completed' }),
  ];
  assert.deepEqual(superseded(others, []), []);
});
