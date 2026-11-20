#!/usr/bin/env node
// Turns downloaded benchmark artifacts into results/<date>/summary.json and summary.md.
//
//   node benchmarks/mutation/summarize.mjs <artifacts dir> [--date YYYY-MM-DD] [--runs id,id]
//
// <artifacts dir> holds one directory per job artifact (any depth), each with the
// result.json, <phase>.log and <phase>.json files run-job.mjs writes. Node only.

import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { joinMutants, originalCode, readSupercovMutants } from './supercov-join.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const { values: args, positionals } = parseArgs({
  allowPositionals: true,
  options: { date: { type: 'string' }, runs: { type: 'string' } },
});
const root = positionals[0];
if (!root || !existsSync(root)) throw new Error('usage: summarize.mjs <artifacts dir> [--date YYYY-MM-DD] [--runs id,id]');
const date = args.date ?? new Date().toISOString().slice(0, 10);
const corpus = JSON.parse(readFileSync(join(here, 'corpus.json'), 'utf8'));

const DETECTED = new Set(['Killed', 'Timeout']);
const UNDETECTED = new Set(['Survived', 'NoCoverage']);
const verdict = (status) => (DETECTED.has(status) ? 'detected' : UNDETECTED.has(status) ? 'undetected' : null);
const stripAnsi = (text) => text.replace(/\x1b\[[0-9;]*m/g, '');

function findResults(dir) {
  const found = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) found.push(...findResults(path));
    else if (name === 'result.json') found.push(path);
  }
  return found;
}

// ---- mutants

/** Mutants of a mutation-testing-elements report, each with a join key. */
function readMutants(file) {
  const report = JSON.parse(readFileSync(file, 'utf8'));
  const mutants = [];
  const testNames = new Map();
  for (const [testFile, tf] of Object.entries(report.testFiles ?? {})) for (const t of tf.tests ?? []) testNames.set(t.id, `${testFile ? `${testFile} > ` : ''}${t.name}`);
  for (const [path, entry] of Object.entries(report.files ?? {})) {
    const filePath = path.replace(/\\/g, '/');
    const source = entry.source ?? null;
    for (const m of entry.mutants) {
      const { start, end } = m.location;
      const span = `${start.line}:${start.column}-${end.line}:${end.column}`;
      const replacement = normalizeReplacement(m.replacement ?? '');
      mutants.push({
        file: filePath,
        span,
        mutator: m.mutatorName,
        replacement,
        rawReplacement: m.replacement ?? '',
        original: originalCode(source, m.location),
        status: m.status,
        static: m.static ?? false,
        killedBy: (m.killedBy ?? []).map((id) => testNames.get(id) ?? id),
        coveredBy: m.coveredBy ?? [],
        statusReason: m.statusReason,
        key: `${filePath}|${span}|${m.mutatorName}|${replacement}`,
        looseKey: `${filePath}|${span}|${replacement}`,
      });
    }
  }
  return mutants;
}

/** The cosmetic differences the mutator benchmark also ignores: outer parentheses, trailing block comments, whitespace. */
function normalizeReplacement(text) {
  let s = text.replace(/\/\*[\s\S]*?\*\/\s*$/g, '').replace(/\s+/g, '');
  while (s.startsWith('(') && s.endsWith(')') && balanced(s.slice(1, -1))) s = s.slice(1, -1);
  return s;
}

function balanced(s) {
  let depth = 0;
  for (const c of s) {
    if (c === '(') depth++;
    else if (c === ')' && --depth < 0) return false;
  }
  return depth === 0;
}

function tally(mutants) {
  const statuses = {};
  let detected = 0;
  let undetected = 0;
  for (const m of mutants) {
    statuses[m.status] = (statuses[m.status] ?? 0) + 1;
    const v = verdict(m.status);
    if (v === 'detected') detected++;
    else if (v === 'undetected') undetected++;
  }
  return { mutants: mutants.length, statuses, score: detected + undetected ? +((100 * detected) / (detected + undetected)).toFixed(2) : null };
}

// ---- logs

/** Seconds into a phase's log at which the first line matching each marker appeared. */
function markers(logFile, patterns) {
  if (!existsSync(logFile)) return {};
  const found = {};
  for (const line of readFileSync(logFile, 'utf8').split('\n')) {
    const m = stripAnsi(line).match(/^\[(\d+\.\d+)\] (.*)$/);
    if (!m) continue;
    for (const [name, re] of Object.entries(patterns)) if (!(name in found) && re.test(m[2])) found[name] = Number(m[1]);
  }
  return found;
}

// Where time goes inside one tool run. Each marker is the first log line of that stage.
const STRYKER_MARKERS = {
  instrumented: /Instrumented \d+ source file/,
  dryRunStart: /Starting initial test run/,
  dryRunEnd: /Initial test run succeeded/,
  mutantsDone: /Done in /,
};
const MUTATOR_MARKERS = {
  dryRunStart: /^dry run: /,
  dryRunEnd: /^dry run finished/,
  plan: /^\d+ mutants, \d+ to run/,
  mutantsDone: /^score/,
};

function stages(wallS, kind, logFile) {
  const m = markers(logFile, kind === 'stryker' ? STRYKER_MARKERS : MUTATOR_MARKERS);
  const r = (x) => +x.toFixed(2);
  if (kind === 'stryker' && m.dryRunStart != null && m.dryRunEnd != null) {
    const done = m.mutantsDone ?? wallS;
    return { setupS: r(m.dryRunStart), dryRunS: r(m.dryRunEnd - m.dryRunStart), mutantsS: r(done - m.dryRunEnd), afterS: r(wallS - done) };
  }
  if (kind === 'mutator' && m.dryRunStart != null && m.plan != null) {
    const done = m.mutantsDone ?? wallS;
    const dryEnd = m.dryRunEnd ?? m.dryRunStart;
    return { setupS: r(m.dryRunStart), dryRunS: r(dryEnd - m.dryRunStart), typecheckAndPlanS: r(m.plan - dryEnd), mutantsS: r(done - m.plan), afterS: r(wallS - done) };
  }
  return null;
}

/** How many mutants a run executed, from its log: Stryker prints the reused count, mutator the executed one. */
function executedCount(kind, logFile) {
  if (!existsSync(logFile)) return null;
  const text = stripAnsi(readFileSync(logFile, 'utf8'));
  if (kind === 'stryker') {
    const instrumented = text.match(/Instrumented \d+ source file\(s\) with (\d+) mutant/);
    const reused = text.match(/(\d+) of (\d+) mutant result\(s\) are reused/);
    const initial = text.match(/Ran (\d+) tests in/);
    if (!instrumented) return null;
    return { instrumented: Number(instrumented[1]), reused: reused ? Number(reused[1]) : 0, dryRunTests: initial ? Number(initial[1]) : null };
  }
  const plan = text.match(/^\[[\d.]+\] (\d+) mutants, (\d+) to run/m);
  const done = text.match(/executed (\d+), reused (\d+)/);
  return { total: plan ? Number(plan[1]) : null, toRun: plan ? Number(plan[2]) : null, executed: done ? Number(done[1]) : null, reused: done ? Number(done[2]) : null };
}

// ---- load every job

const jobs = [];
for (const file of findResults(root)) {
  const dir = dirname(file);
  const result = JSON.parse(readFileSync(file, 'utf8'));
  const phases = {};
  for (const p of result.phases) {
    const kind = p.name.startsWith('stryker') || p.name === 'truth' ? 'stryker' : p.name.startsWith('mutator') ? 'mutator' : null;
    const entry = {
      wallS: p.wallS ?? null,
      cpuS: p.rusage?.cpuS ?? null,
      maxRssMiB: p.rusage ? Math.round(p.rusage.maxRssKiB / 1024) : null,
      exitCode: p.exitCode ?? null,
      timedOut: p.timedOut ?? false,
    };
    if (p.skipped) entry.skipped = p.skipped;
    if (p.tests) entry.tests = p.tests;
    if (kind && p.wallS != null) {
      const log = join(dir, `${p.name}.log`);
      entry.stages = stages(p.wallS, kind, log);
      entry.executed = executedCount(kind, log);
    }
    if (p.mutants && existsSync(join(dir, p.mutants))) {
      const project = corpus.projects.find((x) => x.name === result.project);
      entry._sc = readSupercovMutants(readFileSync(join(dir, p.mutants), 'utf8'), project.mutate);
    }
    if (p.result && existsSync(join(dir, p.result))) {
      try {
        // The test command's own stdout (defu's node:test output) can precede the reply: take its last JSON line.
        const lines = readFileSync(join(dir, p.result), 'utf8').split('\n').filter((l) => l.startsWith('{'));
        const reply = JSON.parse(lines.at(-1));
        entry.reply = reply.summary ? { ...reply.summary, passes: reply.passes ?? null } : null;
      } catch {
        entry.reply = null;
      }
    }
    if (p.report && existsSync(join(dir, p.report))) {
      const mutants = readMutants(join(dir, p.report));
      Object.assign(entry, tally(mutants));
      entry._mutants = mutants;
    }
    phases[p.name] = entry;
  }
  jobs.push({ dir, result, phases });
}

/** A phase without the in-memory mutant list. */
const strip = (phase) => (phase ? Object.fromEntries(Object.entries(phase).filter(([k]) => !k.startsWith('_'))) : null);

/** One phase across the shards of a scenario: summed time, the slowest shard, merged mutants. */
function combine(shardJobs, phaseName) {
  const phases = shardJobs.filter((j) => j.phases[phaseName]).map((j) => ({ shard: j.result.shard, ...j.phases[phaseName] }));
  if (!phases.length) return null;
  const ok = (p) => Boolean(p._mutants) && !p.timedOut && p.exitCode === 0;
  if (phases.length === 1 && !phases[0].shard) return { ...phases[0], complete: ok(phases[0]) };
  const merged = new Map();
  for (const p of phases) {
    for (const m of p._mutants ?? []) {
      const seen = merged.get(m.key);
      if (!seen || (seen.status === 'Pending' && m.status !== 'Pending')) merged.set(m.key, m);
    }
  }
  const mutants = [...merged.values()];
  const sum = (k) => +phases.reduce((s, p) => s + (p[k] ?? 0), 0).toFixed(2);
  return {
    shards: phases.map((p) => strip(p)),
    wallS: sum('wallS'),
    slowestShardS: Math.max(...phases.map((p) => p.wallS ?? 0)),
    cpuS: sum('cpuS'),
    maxRssMiB: Math.max(...phases.map((p) => p.maxRssMiB ?? 0)),
    timedOut: phases.some((p) => p.timedOut),
    complete: phases.every(ok),
    ...tally(mutants),
    _mutants: mutants,
  };
}

/** Score of each tool on the mutants both generate, and verdict differences between them. */
function shared(a, b) {
  const byKey = new Map(b.map((m) => [m.key, m]));
  const aKeys = new Set(a.map((m) => m.key));
  let both = 0;
  let aDetected = 0;
  let bDetected = 0;
  let differences = 0;
  for (const m of a) {
    const other = byKey.get(m.key);
    if (!other) continue;
    const va = verdict(m.status);
    const vb = verdict(other.status);
    if (!va || !vb) continue;
    both++;
    if (va === 'detected') aDetected++;
    if (vb === 'detected') bDetected++;
    if (va !== vb) differences++;
  }
  const looseB = new Set(b.map((m) => m.looseKey));
  return {
    shared: both,
    sameSpanAndReplacement: a.filter((m) => looseB.has(m.looseKey)).length,
    strykerOnly: a.filter((m) => !byKey.has(m.key)).length,
    mutatorOnly: b.filter((m) => !aKeys.has(m.key)).length,
    scoreStryker: both ? +((100 * aDetected) / both).toFixed(2) : null,
    scoreMutator: both ? +((100 * bDetected) / both).toFixed(2) : null,
    verdictDifferences: differences,
  };
}

/** Verdicts of a run against the ground truth, on the mutants both judged. */
function againstTruth(mutants, truth) {
  if (!mutants || !truth) return null;
  const byKey = new Map(truth.map((m) => [m.key, m]));
  const out = { compared: 0, missedKills: 0, extraKills: 0, notInTruth: 0, examples: [] };
  for (const m of mutants) {
    const t = byKey.get(m.key);
    if (!t) {
      out.notInTruth++;
      continue;
    }
    const v = verdict(m.status);
    const vt = verdict(t.status);
    if (!v || !vt) continue;
    out.compared++;
    if (v === vt) continue;
    if (v === 'undetected') out.missedKills++;
    else out.extraKills++;
    if (out.examples.length < 15) {
      out.examples.push({ file: m.file, span: m.span, mutator: m.mutator, replacement: m.replacement, status: m.status, truth: t.status, truthKillers: t.killedBy.length, statusReason: m.statusReason });
    }
  }
  return out;
}

function killMatrix(mutants) {
  if (!mutants) return null;
  const killed = mutants.filter((m) => m.status === 'Killed');
  const killers = killed.map((m) => m.killedBy.length).sort((a, b) => a - b);
  const covered = mutants.filter((m) => m.coveredBy.length);
  return {
    killedMutants: killed.length,
    meanKillersPerKilledMutant: killers.length ? +(killers.reduce((a, b) => a + b, 0) / killers.length).toFixed(2) : null,
    medianKillers: killers.length ? killers[Math.floor(killers.length / 2)] : null,
    killedByOneTestOnly: killers.filter((n) => n === 1).length,
    distinctKillingTests: new Set(killed.flatMap((m) => m.killedBy)).size,
    meanCoveringTests: covered.length ? +(covered.reduce((a, m) => a + m.coveredBy.length, 0) / covered.length).toFixed(2) : null,
  };
}

// ---- Supercov

const SC_DETECTED = new Set(['Killed', 'Timeout']);
const SC_UNDETECTED = new Set(['Survived', 'NoCoverage']);
const scVerdict = (status) => (SC_DETECTED.has(status) ? 'detected' : SC_UNDETECTED.has(status) ? 'undetected' : null);

function scTally(mutants) {
  const statuses = {};
  let detected = 0;
  let undetected = 0;
  for (const m of mutants) {
    statuses[m.status] = (statuses[m.status] ?? 0) + 1;
    const v = scVerdict(m.status);
    if (v === 'detected') detected++;
    else if (v === 'undetected') undetected++;
  }
  return { mutants: mutants.length, statuses, score: detected + undetected ? +((100 * detected) / (detected + undetected)).toFixed(2) : null };
}

/** Supercov's cold run (measure once per job, strong pass summed over shards), its PR run, and the join with the ground truth. */
function supercovSummary(coldJobs, prJob, truth) {
  if (!coldJobs.length && !prJob) return null;
  const out = {};
  if (coldJobs.length) {
    const measures = coldJobs.map((j) => j.phases['supercov-measure']).filter(Boolean);
    const strongs = coldJobs.map((j) => ({ shard: j.result.shard, ...(j.phases['supercov-strong'] ?? {}) }));
    const merged = new Map();
    for (const p of strongs) {
      for (const m of p._sc ?? []) {
        const seen = merged.get(m.id);
        if (!seen || (!seen.strong && m.strong)) merged.set(m.id, m);
      }
    }
    const mutants = [...merged.values()];
    const measureS = measures[0]?.wallS ?? null;
    const strongS = +strongs.reduce((a, p) => a + (p.wallS ?? 0), 0).toFixed(2);
    out.cold = {
      shards: strongs.length,
      measureS,
      measureCpuS: measures[0]?.cpuS ?? null,
      measureRssMiB: measures[0]?.maxRssMiB ?? null,
      measureExitCodes: measures.map((m) => m.exitCode),
      strongS,
      strongCpuS: +strongs.reduce((a, p) => a + (p.cpuS ?? 0), 0).toFixed(2),
      slowestStrongS: Math.max(0, ...strongs.map((p) => p.wallS ?? 0)),
      totalS: measureS == null ? null : +(measureS + strongS).toFixed(2),
      strongExitCodes: strongs.map((p) => p.exitCode),
      timedOut: strongs.some((p) => p.timedOut) || measures.some((m) => m.timedOut),
      complete: strongs.every((p) => p._sc && p.exitCode === 0 && !p.timedOut),
      replies: strongs.map((p) => p.reply ?? null),
      ran: strongs.reduce((a, p) => a + (p.reply?.ran ?? 0), 0),
      passes: strongs.reduce((a, p) => a + (p.reply?.passes?.length ?? 0), 0),
      stalledPasses: strongs.reduce((a, p) => a + (p.reply?.passes ?? []).filter((x) => x.stalled).length, 0),
      shardsWithResumedPasses: strongs.filter((p) => (p.reply?.passes?.length ?? 0) > 1).map((p) => p.shard ?? 'all'),
      ...scTally(mutants),
    };
    out._statuses = new Map(mutants.map((m) => [m.id, m.status]));
    out.cold.strongMsPerMutantRun = out.cold.ran ? Math.round((1000 * strongS) / out.cold.ran) : null;
    out.cold.strongCpuMsPerMutantRun = out.cold.ran ? Math.round((1000 * out.cold.strongCpuS) / out.cold.ran) : null;
    out.vsTruth = supercovAgainstTruth(mutants, truth);
  }
  if (prJob) {
    const m = prJob.phases['supercov-measure'];
    const d = prJob.phases['supercov-diff'];
    const ran = (d?._sc ?? []).filter((x) => x.strong);
    out.pr = {
      measureS: m?.wallS ?? null,
      diffS: d?.wallS ?? null,
      totalS: m?.wallS != null && d?.wallS != null ? +(m.wallS + d.wallS).toFixed(2) : null,
      exitCodes: [m?.exitCode, d?.exitCode],
      diffMutants: ran.length,
      reply: d?.reply ?? null,
      ...Object.fromEntries(Object.entries(scTally(ran)).filter(([k]) => k !== 'mutants')),
    };
  }
  return out;
}

function supercovAgainstTruth(mutants, truth) {
  if (!truth) return null;
  const { pairs, supercovOnly, strykerOnly, ambiguous } = joinMutants(mutants, truth);
  let both = 0;
  let scDetected = 0;
  let truthDetected = 0;
  const violations = [];
  const extraKills = [];
  const notCompared = {};
  for (const [s, t] of pairs) {
    const vs = scVerdict(s.status);
    const vt = scVerdict(t.status);
    if (!vs || !vt) {
      const key = `${s.status} / ${t.status}`;
      notCompared[key] = (notCompared[key] ?? 0) + 1;
      if (s.status === 'Equivalent' && vt === 'detected') violations.push(describe(s, t, 'Supercov calls it equivalent'));
      continue;
    }
    both++;
    if (vs === 'detected') scDetected++;
    if (vt === 'detected') truthDetected++;
    if (vs === vt) continue;
    if (vs === 'undetected') violations.push(describe(s, t));
    else extraKills.push(describe(s, t));
  }
  const staticPairs = pairs.filter(([, t]) => t.static);
  return {
    joined: pairs.length,
    supercovOnly: supercovOnly.length,
    supercovOnlyByMutator: countBy(supercovOnly, (m) => m.mutator),
    strykerOnly: strykerOnly.length,
    strykerOnlyByMutator: countBy(strykerOnly, (m) => m.mutator),
    ambiguous: ambiguous.length,
    compared: both,
    scoreSupercov: both ? +((100 * scDetected) / both).toFixed(2) : null,
    scoreTruth: both ? +((100 * truthDetected) / both).toFixed(2) : null,
    notCompared,
    soundnessViolations: violations,
    extraKills,
    static: {
      joined: staticPairs.length,
      supercovStatuses: countBy(staticPairs, ([s]) => s.status),
      truthStatuses: countBy(staticPairs, ([, t]) => t.status),
    },
  };
}

function describe(s, t, note) {
  return {
    id: s.id,
    supercov: s.status,
    truth: t.status,
    static: t.static,
    truthKillers: t.killedBy.slice(0, 5),
    truthKillerCount: t.killedBy.length,
    supercovKilledBy: s.killedBy.slice(0, 3),
    weak: s.weak,
    weakCode: s.weakCode,
    reason: s.reason,
    original: s.original,
    replacement: t.rawReplacement,
    ...(note ? { note } : {}),
  };
}

function countBy(list, key) {
  const counts = {};
  for (const item of list) counts[key(item)] = (counts[key(item)] ?? 0) + 1;
  return counts;
}

// ---- per project

const SETUP_PHASES = ['clone', 'pnpm-setup', 'install', 'vitest-plugins-remove', 'vitest-upgrade', 'tool-install', 'tests'];
const jobName = (j) => j.result.scenario + (j.result.shard ? `:${j.result.shard}` : '') + (j.result.variant ? `:${j.result.variant}` : '');
const summary = { date, runs: args.runs?.split(',') ?? [], tools: corpus.tools, projects: {} };

for (const project of corpus.projects) {
  const mine = jobs.filter((j) => j.result.project === project.name);
  if (!mine.length) continue;
  const of = (scenario) => mine.filter((j) => j.result.scenario === scenario);

  const truth = combine(of('truth'), 'truth');
  const expectedTruthShards = project.shards?.length ?? 1;
  const stryker = combine(of('stryker-cold'), 'stryker-cold');
  const mutator = combine(of('mutator-cold'), 'mutator-cold');
  const ps = of('pr-stryker')[0];
  const pm = of('pr-mutator')[0];
  const testsOf = (scenario) => of(scenario).find((j) => j.phases.tests)?.phases.tests;

  const main = project.supercovMainVariant ?? null;
  const plain = (list) => list.filter((j) => (j.result.variant ?? null) === main);
  // The PR scenario is not run per strong-pass setting: its plain job goes with the main setting.
  const prJob = plain(of('pr-supercov'))[0] ?? of('pr-supercov').find((j) => !j.result.variant);
  const supercov = supercovSummary(plain(of('supercov-cold')), prJob, truth?._mutants);
  if (supercov) {
    for (const variant of new Set(mine.map((j) => j.result.variant).filter((v) => v && v !== main))) {
      const v = (list) => list.filter((j) => j.result.variant === variant);
      supercov.variants = { ...supercov.variants, [variant]: supercovSummary(v(of('supercov-cold')), v(of('pr-supercov'))[0], truth?._mutants) };
    }
    if (main) supercov.mainVariant = main;
    // Verdicts across settings: the same mutant, a different status.
    const comparisons = {};
    for (const [variant, other] of Object.entries(supercov.variants ?? {})) {
      if (!supercov._statuses || !other?._statuses) continue;
      let compared = 0;
      const differ = [];
      for (const [id, status] of supercov._statuses) {
        const o = other._statuses.get(id);
        if (!o || status === 'Pending' || o === 'Pending') continue;
        compared++;
        if (o !== status) differ.push({ id, [main ?? 'main']: status, [variant]: o });
      }
      comparisons[variant] = { compared, differ: differ.length, examples: differ.slice(0, 20) };
    }
    if (Object.keys(comparisons).length) supercov.verdictsAcrossVariants = comparisons;
    delete supercov._statuses;
    for (const other of Object.values(supercov.variants ?? {})) if (other) delete other._statuses;
  }

  summary.projects[project.name] = {
    commit: project.commit,
    machines: [...new Set(mine.map((j) => j.result.machine?.instanceType).filter(Boolean))],
    cpuModel: mine[0].result.machine?.cpuModel ?? null,
    versions: Object.fromEntries(mine.map((j) => [jobName(j), j.result.versions])),
    notes: [...new Set(mine.flatMap((j) => j.result.notes ?? []))],
    errors: mine.filter((j) => j.result.error).map((j) => ({ job: jobName(j), error: j.result.error.split('\n')[0] })),
    setup: Object.fromEntries(mine.map((j) => [jobName(j), Object.fromEntries(j.result.phases.filter((p) => SETUP_PHASES.includes(p.name)).map((p) => [p.name, p.wallS]))])),
    plainTests: { strykerVitest: strip(testsOf('stryker-cold')), mutatorVitest: strip(testsOf('mutator-cold')) },
    cold: { stryker: strip(stryker), mutator: strip(mutator) },
    sharedCold: stryker?._mutants && mutator?._mutants ? shared(stryker._mutants, mutator._mutants) : null,
    truth: truth
      ? { ...strip(truth), complete: Boolean(truth.complete) && of('truth').length === expectedTruthShards, expectedShards: expectedTruthShards, killMatrix: killMatrix(truth._mutants) }
      : null,
    vsTruth: { stryker: againstTruth(stryker?._mutants, truth?._mutants), mutator: againstTruth(mutator?._mutants, truth?._mutants) },
    supercov,
    pr: {
      changedLines: ps?.result.changedLines ?? null,
      strykerIncrementalBase: strip(ps?.phases['stryker-incremental-base']),
      strykerLines: strip(ps?.phases['stryker-lines']),
      strykerIncremental: strip(ps?.phases['stryker-incremental']),
      mutatorBase: strip(pm?.phases['mutator-base']),
      mutatorSinceCold: strip(pm?.phases['mutator-since-cold']),
      mutatorSinceWarm: strip(pm?.phases['mutator-since-warm']),
    },
  };
}

// ---- write

const outDir = join(here, 'results', date);
mkdirSync(join(outDir, 'jobs'), { recursive: true });
for (const j of jobs) {
  const name = `${j.result.project}-${j.result.scenario}${j.result.shard ? `-${j.result.shard}` : ''}.json`;
  copyFileSync(join(j.dir, 'result.json'), join(outDir, 'jobs', name));
}
writeFileSync(join(outDir, 'summary.json'), `${JSON.stringify(summary, null, 2)}\n`);
writeFileSync(join(outDir, 'summary.md'), markdown(summary));
console.log(`wrote ${relative(process.cwd(), outDir)}/summary.json and summary.md (${jobs.length} jobs)`);

// ---- markdown

function fmt(n, digits = 1) {
  return n == null || Number.isNaN(Number(n)) ? '–' : Number(n).toFixed(digits);
}

function statusText(statuses) {
  if (!statuses || !Object.keys(statuses).length) return '–';
  const short = { Killed: 'K', Timeout: 'T', Survived: 'S', NoCoverage: 'NC', RuntimeError: 'RE', CompileError: 'CE', Ignored: 'I', Pending: 'P' };
  return Object.entries(statuses)
    .sort((a, b) => b[1] - a[1])
    .map(([s, n]) => `${short[s] ?? s}${n}`)
    .join(' ');
}

function wallText(r) {
  if (!r) return '–';
  const base = r.shards ? `${fmt(r.wallS)} (${r.shards.length} shards, slowest ${fmt(r.slowestShardS)})` : fmt(r.wallS);
  return r.complete === false ? `${base}, incomplete` : base;
}

function markdown(s) {
  const L = [];
  L.push(`# Mutation benchmark ${s.date}`, '');
  if (s.runs.length) L.push(`CI runs: ${s.runs.join(', ')}.`, '');
  L.push(
    `StrykerJS ${s.tools.stryker}, mizchi/mutator ${s.tools.mutator}, Node ${s.tools.node}. Statuses: K Killed, T Timeout, S Survived, NC NoCoverage, RE RuntimeError, CE CompileError, I Ignored, P Pending. Score = (K+T) / (K+T+S+NC). Sharded runs: wall and CPU seconds are summed over the shards.`,
    '',
  );

  L.push('## Cold full runs', '');
  L.push('| project | tests | tool | wall s | CPU s | peak RSS MiB | mutants | statuses | score | shared mutants | score on shared | vs ground truth: missed kills / extra kills (of compared) |');
  L.push('|---|---:|---|---|---:|---:|---:|---|---:|---:|---:|---|');
  for (const [name, p] of Object.entries(s.projects)) {
    const tests = p.plainTests.strykerVitest?.tests?.total ?? p.plainTests.mutatorVitest?.tests?.total ?? '–';
    for (const tool of ['stryker', 'mutator']) {
      const r = p.cold[tool];
      const sh = p.sharedCold;
      const vt = p.vsTruth[tool];
      const sharedScore = sh ? fmt(tool === 'stryker' ? sh.scoreStryker : sh.scoreMutator) : '–';
      L.push(
        `| ${name} | ${tests} | ${tool === 'stryker' ? 'StrykerJS' : 'mutator'} | ${wallText(r)} | ${fmt(r?.cpuS, 0)} | ${r?.maxRssMiB ?? '–'} | ${r?.mutants ?? '–'} | ${statusText(r?.statuses)} | ${fmt(r?.score)} | ${sh ? sh.shared : '–'} | ${sharedScore} | ${vt ? `${vt.missedKills} / ${vt.extraKills} (of ${vt.compared})` : '–'} |`,
      );
    }
  }
  L.push('');

  L.push('## Ground truth (StrykerJS, disableBail)', '');
  L.push('| project | shards | complete | wall s (sum) | CPU s | mutants | statuses | score | mean killers per killed mutant | killed by one test only | distinct killing tests |');
  L.push('|---|---|---|---:|---:|---:|---|---:|---:|---:|---:|');
  for (const [name, p] of Object.entries(s.projects)) {
    const t = p.truth;
    if (!t) continue;
    const km = t.killMatrix ?? {};
    L.push(
      `| ${name} | ${t.shards?.length ?? 1} of ${t.expectedShards} | ${t.complete ? 'yes' : 'no'} | ${fmt(t.wallS)} | ${fmt(t.cpuS, 0)} | ${t.mutants} | ${statusText(t.statuses)} | ${fmt(t.score)} | ${fmt(km.meanKillersPerKilledMutant, 2)} | ${km.killedByOneTestOnly ?? '–'} | ${km.distinctKillingTests ?? '–'} |`,
    );
  }
  L.push('');

  L.push('## PR scenario (one scripted edit)', '');
  L.push(
    'StrykerJS cold = `mutate` restricted to the changed lines, no incremental file. StrykerJS warm = the PR scope with `--incremental` from the pre-edit run. mutator = `--since HEAD~1` with the edit committed, without (cold) and with (warm) the pre-edit snapshot. Executed = mutants the run actually ran (StrykerJS: instrumented minus reused). Statuses leave out Pending (outside the diff).',
    '',
  );
  L.push('| project | changed lines | Stryker cold: s / mutants / statuses | Stryker warm: s / executed | mutator cold: s / executed / statuses | mutator warm: s / executed / statuses | base runs: Stryker / mutator s |');
  L.push('|---|---|---|---|---|---|---|');
  for (const [name, p] of Object.entries(s.projects)) {
    const r = p.pr;
    if (!r.strykerLines && !r.mutatorSinceCold) continue;
    const strykerExecuted = (x) => (x?.executed ? x.executed.instrumented - x.executed.reused : '–');
    const diffStatuses = (x) => statusText(Object.fromEntries(Object.entries(x?.statuses ?? {}).filter(([k]) => k !== 'Pending')));
    const strykerCold = r.strykerLines ? `${fmt(r.strykerLines.wallS)} / ${r.strykerLines.mutants ?? '–'} / ${statusText(r.strykerLines.statuses)}` : '–';
    const strykerWarm = r.strykerIncremental ? `${fmt(r.strykerIncremental.wallS)} / ${strykerExecuted(r.strykerIncremental)}` : '–';
    const mutatorRun = (x) => (x ? `${fmt(x.wallS)} / ${x.executed?.executed ?? '–'} / ${diffStatuses(x)}` : '–');
    L.push(
      `| ${name} | ${(r.changedLines ?? []).join(', ')} | ${strykerCold} | ${strykerWarm} | ${mutatorRun(r.mutatorSinceCold)} | ${mutatorRun(r.mutatorSinceWarm)} | ${fmt(r.strykerIncrementalBase?.wallS)} / ${fmt(r.mutatorBase?.wallS)} |`,
    );
  }
  L.push('');

  L.push('## Where the time goes (cold runs)', '');
  L.push(
    'StrykerJS: set-up = start to "Starting initial test run" (instrumenting, sandbox copy, starting test-runner processes); dry run = the initial test run with per-test coverage; mutants = until "Done in". mutator: set-up = start to "dry run:"; dry run = until "dry run finished"; type check + plan = until "N mutants, M to run" (the TypeScript check of every mutant, on by default); mutants = until the score line. Sharded runs show their first shard.',
    '',
  );
  L.push("| project | install s | plain tests s (Stryker's / mutator's Vitest) | Stryker set-up / dry run / mutants s | mutator set-up / dry run / type check + plan / mutants s |");
  L.push('|---|---:|---|---|---|');
  for (const [name, p] of Object.entries(s.projects)) {
    const st = p.cold.stryker?.stages ?? p.cold.stryker?.shards?.[0]?.stages;
    const mt = p.cold.mutator?.stages ?? p.cold.mutator?.shards?.[0]?.stages;
    const install = Object.values(p.setup).find((x) => x.install != null)?.install;
    L.push(
      `| ${name} | ${fmt(install)} | ${fmt(p.plainTests.strykerVitest?.wallS)} / ${fmt(p.plainTests.mutatorVitest?.wallS)} | ${st ? `${fmt(st.setupS)} / ${fmt(st.dryRunS)} / ${fmt(st.mutantsS)}` : '–'} | ${mt ? `${fmt(mt.setupS)} / ${fmt(mt.dryRunS)} / ${fmt(mt.typecheckAndPlanS)} / ${fmt(mt.mutantsS)}` : '–'} |`,
    );
  }
  L.push('');

  L.push(...supercovMarkdown(s));

  const problems = Object.entries(s.projects).flatMap(([name, p]) => [...p.errors.map((e) => `${name} ${e.job}: ${e.error}`), ...p.notes.map((n) => `${name}: ${n}`)]);
  if (problems.length) L.push('## Notes and errors', '', ...problems.map((x) => `- ${x}`), '');
  return L.join('\n');
}

function supercovMarkdown(s) {
  const projects = Object.entries(s.projects).filter(([, p]) => p.supercov);
  if (!projects.length) return [];
  const L = [];
  L.push('## Supercov (strong mutation)', '');
  L.push(
    'Cold: `supercov -- <test command>` (measure: per-test coverage and weak mutation), then `supercov runs latest mutants test` (strong pass); a sharded strong pass is summed over its `--shard k/n` jobs, each of which measured again (the measure time shown is one job\'s). PR: the edit committed, measure, then `mutants test --diff HEAD~1`. Statuses: K killed, S survived, T timeout, NC no-coverage, RE error, Equivalent (static rule), P not run. Only mutants in the corpus mutate set are counted.',
    '',
  );
  L.push('| project | measure s | strong s | total s | StrykerJS cold s | mutants | statuses | score | joined to truth | score on joined (Supercov / truth) | soundness violations | extra kills | PR: measure / diff s, mutants run of the diff, statuses |');
  L.push('|---|---:|---:|---:|---:|---:|---|---:|---:|---|---:|---:|---|');
  const rows = projects.flatMap(([name, p]) => [[p.supercov.mainVariant ? `${name} (${p.supercov.mainVariant})` : name, p.supercov], ...Object.entries(p.supercov.variants ?? {}).map(([v, sc]) => [`${name} (${v})`, sc])]);
  const artifacts = (name) => corpus.projects.find((x) => x.name === name.split(' ')[0])?.groundTruthArtifacts ?? {};
  for (const [name, sc] of rows) {
    const p = s.projects[name.split(' ')[0]];
    const c = sc.cold;
    const v = sc.vsTruth;
    const pr = sc.pr;
    const strong = c ? `${fmt(c.strongS)}${c.shards > 1 ? ` (${c.shards} shards, slowest ${fmt(c.slowestStrongS)})` : ''}${c.complete ? '' : ', incomplete'}` : '–';
    L.push(
      `| ${name} | ${fmt(c?.measureS)} | ${strong} | ${fmt(c?.totalS)} | ${fmt(p.cold.stryker?.wallS)} | ${c?.mutants ?? '–'} | ${statusText(c?.statuses)} | ${fmt(c?.score)} | ${v ? `${v.joined} (Supercov only ${v.supercovOnly}, StrykerJS only ${v.strykerOnly})` : '–'} | ${v ? `${fmt(v.scoreSupercov)} / ${fmt(v.scoreTruth)} (${v.compared})` : '–'} | ${v ? `${v.soundnessViolations.length}${v.soundnessViolations.some((d) => artifacts(name)[d.id]) ? ` (${v.soundnessViolations.filter((d) => !artifacts(name)[d.id]).length} after diagnosis)` : ''}` : '–'} | ${v ? `${v.extraKills.length}${v.extraKills.some((d) => artifacts(name)[d.id]?.startsWith('supercov-false-kill')) ? ` (${v.extraKills.filter((d) => artifacts(name)[d.id]?.startsWith('supercov-false-kill')).length} diagnosed false)` : ''}` : '–'} | ${pr ? `${fmt(pr.measureS)} / ${fmt(pr.diffS)}, ${pr.reply ? `${pr.reply.ran} of ${pr.reply.mutants}, K${pr.reply.killed} T${pr.reply.timeout} S${pr.reply.survived} NC${pr.reply.noCoverage}${pr.reply.error ? ` RE${pr.reply.error}` : ''}` : `${pr.diffMutants}, ${statusText(pr.statuses)}`}` : '–'} |`,
    );
  }
  L.push('');
  L.push('### Where Supercov\'s time goes', '');
  L.push('Strong pass per mutant = strong-pass wall (CPU) seconds over the mutants it ran (not-infected, not-reached and equivalent mutants are decided without running). StrykerJS per mutant = its cold mutant phase over its mutants, for scale.', '');
  L.push('| project | plain tests s | measure s | strong pass s | mutants run | strong ms per mutant run (wall / CPU) | StrykerJS mutant phase ms per mutant | strong passes (stalled) |');
  L.push('|---|---:|---:|---:|---:|---|---:|---|');
  for (const [name, sc] of rows) {
    const p = s.projects[name.split(' ')[0]];
    const c = sc.cold;
    if (!c) continue;
    const st = p.cold.stryker;
    const stMutants = st?.stages?.mutantsS ?? null;
    const perStryker = st?.mutants && stMutants != null ? Math.round((1000 * stMutants) / st.mutants) : st?.shards ? Math.round((1000 * st.shards.reduce((a, x) => a + (x.stages?.mutantsS ?? 0), 0)) / st.mutants) : null;
    L.push(`| ${name} | ${fmt(p.plainTests.strykerVitest?.wallS ?? p.plainTests.mutatorVitest?.wallS)} | ${fmt(c.measureS)} | ${fmt(c.strongS)} | ${c.ran} | ${c.strongMsPerMutantRun ?? '–'} / ${c.strongCpuMsPerMutantRun ?? '–'} | ${perStryker ?? '–'} | ${c.passes} (${c.stalledPasses})${c.shardsWithResumedPasses.length ? `, resumed in ${c.shardsWithResumedPasses.join(', ')}` : ''} |`);
  }
  L.push('');
  const across = projects.filter(([, p]) => p.supercov.verdictsAcrossVariants);
  if (across.length) {
    L.push('### Verdicts across strong-pass settings', '');
    L.push('| project | compared with | mutants compared | different verdicts |');
    L.push('|---|---|---:|---:|');
    for (const [name, p] of across) for (const [variant, c] of Object.entries(p.supercov.verdictsAcrossVariants)) L.push(`| ${name}${p.supercov.mainVariant ? ` (${p.supercov.mainVariant})` : ''} | ${variant} | ${c.compared} | ${c.differ}${c.examples.length ? `: ${c.examples.slice(0, 5).map((e) => `\`${e.id}\` ${Object.values(e).slice(1).join(' / ')}`).join('; ')}` : ''} |`);
    L.push('');
  }
  L.push('### Module-level (static) mutants', '');
  L.push('StrykerJS\'s ground truth runs static mutants (no `ignoreStatic`); Supercov switches them on before the module loads.', '');
  L.push('| project | static mutants joined | Supercov statuses | ground-truth statuses |');
  L.push('|---|---:|---|---|');
  for (const [name, sc] of rows) {
    const st = sc.vsTruth?.static;
    if (!st) continue;
    L.push(`| ${name} | ${st.joined} | ${statusText(st.supercovStatuses)} | ${statusText(st.truthStatuses)} |`);
  }
  L.push('');
  L.push('### Every disagreement with the ground truth', '');
  L.push('A soundness violation is a mutant the ground truth detects (Killed or Timeout) that Supercov calls survived, no-coverage or equivalent. An extra kill is the reverse.', '');
  let any = false;
  for (const [name, sc] of rows) {
    const v = sc.vsTruth;
    if (!v) continue;
    for (const [kind, list] of [['soundness violation', v.soundnessViolations], ['extra kill', v.extraKills]]) {
      for (const d of list) {
        any = true;
        const killers = d.truthKillerCount ? `; ground truth killed by ${d.truthKillerCount} (${d.truthKillers.map((k) => `\`${k}\``).join(', ')}${d.truthKillerCount > d.truthKillers.length ? ', ...' : ''})` : '';
        const artifact = corpus.projects.find((x) => x.name === name.split(' ')[0])?.groundTruthArtifacts?.[d.id];
        const artifactLabel = !artifact ? '' : artifact.startsWith('flaky') ? ' (ground truth: flaky kill)' : artifact.startsWith('supercov-false-kill') ? ' (Supercov false kill)' : ' (ground-truth artifact)';
        L.push(`- ${name}, ${kind}${artifactLabel}${d.static ? ' (static)' : ''}: \`${d.id}\`: Supercov ${d.supercov}${d.weakCode ? ` (weak: ${d.weak}, ${d.weakCode})` : ''}${d.reason ? `, "${String(d.reason).slice(0, 160)}"` : ''}; ground truth ${d.truth}${killers}${d.note ? `; ${d.note}` : ''}${artifact ? `. Diagnosed: ${artifact}` : ''}`);
      }
    }
  }
  if (!any) L.push('None.');
  L.push('');
  return L;
}
