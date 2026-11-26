#!/usr/bin/env node
// The two tiers of CI, and the commit statuses that say a commit passed them.
//
// The fast tier (ci.yml) runs on every push: build, lint and the tests the
// change can affect, in about ten minutes. The slow tier (slow-tier.yml) runs
// Self Coverage, Compatibility, React, Next.js and the release gates once a
// pull request is ready to merge, each night on main, and on every push to a
// release pull request. Each tier's answer is a commit status, `Fast tier` and
// `Slow tier`, which branch protection requires and publish.yml reads; not a
// check of a job's name, which a run that decided to run nothing (a label's)
// leaves as skipped, and a skipped check counts as passed.
//
//   node scripts/ci-tiers.mjs decide
//     Whether this event runs the slow tier, in full or on the changed lines,
//     as the step outputs run, full and reason. A push it does not run on is
//     marked pending ("not run on this commit"), and a `ready` label it
//     outlived is taken off, so the pull request says what to do.
//   node scripts/ci-tiers.mjs report <context>
//     The status of <context> for SHA, from RESULTS (the `needs` of the job,
//     as JSON): success when every job passed or was not needed, failure
//     when one failed or was cancelled. FULL says whether every test ran:
//     only a status that says "Passed in full" lets a release publish.
//   node scripts/ci-tiers.mjs verify-release [<commit>]
//     Whether the tree of <commit> (HEAD) passed both tiers in full: on that
//     commit, or on another of the same tree (the release pull request's head,
//     whose squash merge it is). Exits 1, naming what is missing, when not.
import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, readFileSync } from 'node:fs';

export const FAST = 'Fast tier';
export const SLOW = 'Slow tier';
export const READY_LABEL = 'ready';
export const FULL_LABEL = 'full-ci';
// What a status says when every test ran, as a release requires.
export const IN_FULL = 'Passed in full';
// Statuses only GitHub Actions posts count: anyone who can push can post one.
const ACTIONS = 'github-actions[bot]';

const isRelease = (pull) => /^Release \d+\.\d+\.\d+/u.test(pull.title ?? '') || /^release\//u.test(pull.head?.ref ?? '');

/**
 * Whether an event runs the slow tier: {run, full, reason}, and for a pull
 * request event it does not run on, the pending status to set (`pending`),
 * whether to take a stale `ready` label off (`stale`), or to leave the
 * commit's status as it is (`quiet`: a label that asks for nothing).
 */
export function decide(name, payload = {}) {
  if (name === 'schedule') return { run: true, full: true, reason: 'the nightly run of main' };
  if (name === 'workflow_dispatch') return { run: true, full: true, reason: 'a run by hand measures in full' };
  if (name !== 'pull_request') return { run: false, quiet: true, reason: `a ${name} event runs no slow tier` };
  const pull = payload.pull_request ?? {};
  const labels = new Set((pull.labels ?? []).map((label) => label.name));
  const release = isRelease(pull);
  const full = release || labels.has(FULL_LABEL);
  const action = payload.action;
  if (action === 'labeled' && ![READY_LABEL, FULL_LABEL].includes(payload.label?.name)) {
    return { run: false, quiet: true, reason: `the label ${payload.label?.name} asks for nothing here` };
  }
  if (pull.draft) {
    return { run: false, pending: 'A draft runs the fast tier alone: mark it ready for review to run the slow tier', reason: 'a draft' };
  }
  const run = (reason) => ({ run: true, full, reason: `${reason}${full ? `, in full (${release ? 'a release pull request' : `the \`${FULL_LABEL}\` label`})` : ', on the changed lines'}` });
  if (action === 'labeled') return run(`the \`${payload.label.name}\` label`);
  if (action === 'ready_for_review') return run('ready for review');
  if (release) return run('a push to a release pull request');
  if (labels.has(READY_LABEL) && action !== 'synchronize') return run(`opened with the \`${READY_LABEL}\` label`);
  return {
    run: false,
    pending: `Not run on this commit: add the \`${READY_LABEL}\` label when it is ready to merge`,
    stale: action === 'synchronize' && labels.has(READY_LABEL),
    reason: action === 'synchronize' ? 'a push to a pull request runs the fast tier alone' : 'not marked ready to merge',
  };
}

/** A tier's commit status from its jobs' results: {state, description}. */
export function verdict(results, full) {
  const failed = Object.entries(results ?? {})
    .filter(([, need]) => need.result === 'failure' || need.result === 'cancelled')
    .map(([job, need]) => `${job} ${need.result === 'cancelled' ? 'was cancelled' : 'failed'}`);
  if (Object.keys(results ?? {}).length === 0) return { state: 'failure', description: 'No job ran' };
  if (failed.length > 0) return { state: 'failure', description: failed.join(', ').slice(0, 140) };
  return { state: 'success', description: full ? IN_FULL : 'Passed on what the change can affect' };
}

/**
 * Whether one commit's statuses say it passed both tiers in full: the newest
 * status of each context that GitHub Actions posted (the API lists them
 * newest first). Returns what is missing, or [] when nothing is.
 */
export function missing(statuses) {
  const newest = new Map();
  for (const status of statuses) {
    if (status.creator?.login !== ACTIONS) continue;
    if (!newest.has(status.context)) newest.set(status.context, status);
  }
  const gaps = [];
  for (const context of [FAST, SLOW]) {
    const status = newest.get(context);
    if (!status) gaps.push(`no ${context} status`);
    else if (status.state !== 'success') gaps.push(`${context} is ${status.state}`);
    else if (!status.description?.startsWith(IN_FULL)) gaps.push(`${context} passed on part of the tests only`);
  }
  return gaps;
}

// ------------------------------------------------------------------ GitHub

const repository = () => process.env.GITHUB_REPOSITORY;
const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8', maxBuffer: 1 << 28 });
const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
const runUrl = () => `${process.env.GITHUB_SERVER_URL}/${repository()}/actions/runs/${process.env.GITHUB_RUN_ID}`;

function post(sha, context, state, description) {
  gh('api', `repos/${repository()}/statuses/${sha}`, '-f', `state=${state}`, '-f', `context=${context}`,
    '-f', `description=${description.slice(0, 140)}`, '-f', `target_url=${runUrl()}`, '--silent');
  console.log(`${context} on ${sha}: ${state}, ${description}`);
}

function output(values) {
  const lines = Object.entries(values).map(([key, value]) => `${key}=${value}`).join('\n');
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${lines}\n`);
  console.log(lines);
}

function event() {
  const path = process.env.GITHUB_EVENT_PATH;
  return path && existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {};
}

/** The commits whose statuses could stand for `sha`'s tree. */
function candidates(sha) {
  const found = [sha];
  // The pull request this commit came from: its head is what was verified.
  try {
    for (const head of gh('api', `repos/${repository()}/commits/${sha}/pulls`, '--jq', '.[].head.sha').split('\n')) if (head) found.push(head);
  } catch {
    // None, or none the API knows of: the other candidates still count.
  }
  // Every commit the slow tier ran on lately, a release pull request's among them.
  try {
    for (const head of gh('run', 'list', '--repo', repository(), '--workflow', 'slow-tier.yml', '--limit', '60', '--json', 'headSha', '--jq', '.[].headSha').split('\n')) if (head) found.push(head);
  } catch {
    // The slow tier has not run yet.
  }
  return [...new Set(found)];
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith('ci-tiers.mjs')) {
  const [command, ...rest] = process.argv.slice(2);
  if (command === 'decide') {
    const payload = event();
    const decision = decide(process.env.GITHUB_EVENT_NAME, payload);
    const sha = payload.pull_request?.head?.sha ?? process.env.GITHUB_SHA;
    // Each night measures main again, but for Self Coverage: a push to main
    // measured this commit already, and the map it keeps is the measurement.
    let selfCoverage = decision.run;
    if (process.env.GITHUB_EVENT_NAME === 'schedule') {
      const measured = gh('run', 'list', '--repo', repository(), '--workflow', 'self-coverage.yml', '--commit', sha,
        '--event', 'push', '--status', 'success', '--limit', '1', '--json', 'databaseId', '--jq', 'length').trim();
      if (measured !== '0') selfCoverage = false;
    }
    if (decision.pending) post(sha, SLOW, 'pending', decision.pending);
    if (decision.stale) {
      try {
        gh('pr', 'edit', String(payload.pull_request.number), '--repo', repository(), '--remove-label', READY_LABEL);
        console.log(`Took the \`${READY_LABEL}\` label off: it was given to an earlier commit.`);
      } catch (error) {
        console.log(`The \`${READY_LABEL}\` label could not be taken off: ${error.message}`);
      }
    }
    if (decision.run) post(sha, SLOW, 'pending', `Running: ${decision.reason}`);
    output({ run: decision.run, full: Boolean(decision.full), 'self-coverage': selfCoverage, sha });
    if (process.env.GITHUB_STEP_SUMMARY) {
      appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### Slow tier\n\n${decision.run ? 'Runs' : 'Does not run'}: ${decision.reason}.${selfCoverage || !decision.run ? '' : ' Self Coverage measured this commit on its push already.'}\n`);
    }
  } else if (command === 'report') {
    const [context] = rest;
    const results = JSON.parse(process.env.RESULTS || '{}');
    const { state, description } = verdict(results, process.env.FULL === 'true');
    post(process.env.SHA, context, state, description);
    process.exit(state === 'success' ? 0 : 1);
  } else if (command === 'verify-release') {
    const commit = rest[0] ?? 'HEAD';
    const sha = git('rev-parse', commit);
    const tree = git('rev-parse', `${sha}^{tree}`);
    const tried = [];
    for (const candidate of candidates(sha)) {
      try {
        git('cat-file', '-e', `${candidate}^{commit}`);
      } catch {
        try {
          git('fetch', '-q', 'origin', candidate);
        } catch {
          continue;
        }
      }
      if (git('rev-parse', `${candidate}^{tree}`) !== tree) continue;
      const statuses = gh('api', '--paginate', `repos/${repository()}/commits/${candidate}/statuses?per_page=100`, '--jq', '.[] | @json')
        .split('\n').filter(Boolean).map((line) => JSON.parse(line));
      const gaps = missing(statuses);
      if (gaps.length === 0) {
        console.log(`Tree ${tree} passed both tiers in full on ${candidate}.`);
        process.exit(0);
      }
      tried.push(`${candidate}: ${gaps.join('; ')}`);
    }
    console.error(`Tree ${tree} of ${sha} has not passed both tiers in full on any commit of that tree, so it is not published.`);
    for (const line of tried) console.error(`  ${line}`);
    if (tried.length === 0) console.error('  No commit of this tree has a status from either tier.');
    console.error('Run both tiers on it: gh workflow run ci.yml --ref main && gh workflow run slow-tier.yml --ref main');
    process.exit(1);
  } else {
    console.error('usage: ci-tiers.mjs decide | report <context> | verify-release [<commit>]');
    process.exit(2);
  }
}
