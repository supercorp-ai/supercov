#!/usr/bin/env node
// Cancels the merge queue's superseded runs. When the queue rebuilds its
// groups (a pull request jumps ahead, leaves, or turns unmergeable), each
// group behind gets a new commit and new runs of both tiers, and GitHub
// leaves the old commit's runs queued and running: on 2026-11-28, twelve of
// them held about 110 jobs at once, ahead of the runs the queue waited for.
//
//   node scripts/queue-sweep.mjs
//     Lists this repository's unfinished `merge_group` runs, then the merge
//     queue's entries, and cancels each run whose commit is no entry's
//     commit (nor this event's). Runs are listed before the queue is read,
//     so a group made in between is never cancelled: a run can only be
//     cancelled when the queue had already left its commit. Only runs of a
//     merge group, on a `gh-readonly-queue/` branch, are ever touched.
import { execFileSync } from 'node:child_process';

const QUEUE_BRANCH = 'gh-readonly-queue/';
const UNFINISHED = ['queued', 'in_progress', 'waiting'];

/**
 * The runs to cancel: merge-group runs on a queue branch whose commit is not
 * one the queue still builds (`live`, the entries' commits and this event's).
 */
export function superseded(runs, live) {
  const keep = new Set(live.filter(Boolean));
  const cancel = [];
  for (const run of runs) {
    if (run.event !== 'merge_group') continue;
    if (!String(run.head_branch ?? '').startsWith(QUEUE_BRANCH)) continue;
    if (!UNFINISHED.includes(run.status)) continue;
    if (keep.has(run.head_sha)) continue;
    cancel.push(run);
  }
  return cancel;
}

const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8', maxBuffer: 1 << 26 });

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith('queue-sweep.mjs')) {
  const repository = process.env.GITHUB_REPOSITORY;
  const [owner, name] = repository.split('/');
  const runs = [];
  for (const status of UNFINISHED) {
    const page = JSON.parse(gh('api', `repos/${repository}/actions/runs?event=merge_group&status=${status}&per_page=100`));
    runs.push(...page.workflow_runs);
  }
  // After the runs: a group the queue makes from here on has runs this
  // listing does not hold.
  const queue = JSON.parse(gh('api', 'graphql', '-f', `query=query { repository(owner: "${owner}", name: "${name}") {
    mergeQueue { entries(first: 100) { nodes { headCommit { oid } pullRequest { number } } } } } }`));
  const entries = queue.data.repository.mergeQueue?.entries.nodes ?? [];
  const live = [...entries.map((entry) => entry.headCommit?.oid), process.env.GITHUB_EVENT_NAME === 'merge_group' ? process.env.GITHUB_SHA : ''];
  const cancel = superseded(runs, live);
  console.log(`${runs.length} unfinished merge-group run(s), ${entries.length} queue entr${entries.length === 1 ? 'y' : 'ies'}, ${cancel.length} superseded`);
  for (const run of cancel) {
    try {
      gh('api', '-X', 'POST', `repos/${repository}/actions/runs/${run.id}/cancel`, '--silent');
      console.log(`cancelled ${run.name} ${run.id} on ${run.head_branch} (${run.head_sha.slice(0, 8)}): its commit is no queue entry's`);
    } catch (error) {
      console.log(`could not cancel ${run.id}: ${error.message.split('\n')[0]}`);
    }
  }
  if (process.env.GITHUB_STEP_SUMMARY) {
    execFileSync('sh', ['-c', `cat >> "$GITHUB_STEP_SUMMARY"`], { input: `### Merge queue sweep\n\n${cancel.length} superseded run(s) cancelled of ${runs.length} unfinished.\n` });
  }
}
