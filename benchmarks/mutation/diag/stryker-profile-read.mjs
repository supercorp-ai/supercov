// Reads stryker-forklog.cjs's records (and StrykerJS's output, for when its mutant phase began and
// ended): the Vitest workers of the mutant phase, how long each lived, what it ran, and how long
// each test runner's Vitest waited between one worker's end and the next one's start.
//   node stryker-profile-read.mjs <processes.jsonl> <stryker.out>
import { readFileSync } from 'node:fs';
const [logFile, outFile] = process.argv.slice(2);
const records = readFileSync(logFile, 'utf8').split('\n').filter(Boolean).flatMap((line) => { try { return [JSON.parse(line)]; } catch { return []; } });
const text = readFileSync(outFile, 'utf8').replace(/\x1b\[[0-9;]*m/g, '');
const clock = (pattern) => { const m = text.match(pattern); if (!m) return null; const [h, mi, s] = m[1].split(':').map(Number); return { h, mi, s }; };
const procs = new Map();
for (const r of records) {
  const p = procs.get(r.pid) ?? { pid: r.pid, runs: [] };
  if (r.start) Object.assign(p, { start: r.at, ppid: r.ppid, script: r.script, bootMs: r.up });
  if (r.exit) p.exit = r.at;
  if (r.request === 'run' || r.request === 'collect') p.runs.push({ at: r.at, files: r.files });
  if (r.request === 'start') p.startMsg = r.at;
  procs.set(r.pid, p);
}
const workers = [...procs.values()].filter((p) => /workers\/(forks|vmForks)\.js$/.test(p.script ?? ''));
const firstAt = Math.min(...records.map((r) => r.at));
// The mutant phase: from the dry run's end (its last worker's exit before the first gap) is not
// known from the log alone; StrykerJS prints "Initial test run succeeded" at its end.
const dry = text.match(/(\d\d:\d\d:\d\d) \(\d+\) INFO DryRunExecutor\S* Initial test run succeeded/);
const done = text.match(/(\d\d:\d\d:\d\d) \(\d+\) INFO MutationTestExecutor\S* Done in/);
const toMs = (hms) => { const d = new Date(firstAt); const [h, m, s] = hms.split(':').map(Number); d.setHours(h, m, s, 0); return d.getTime(); };
const phaseStart = dry ? toMs(dry[1]) : firstAt;
const phaseEnd = done ? toMs(done[1]) + 999 : Math.max(...records.map((r) => r.at));
const inPhase = workers.filter((w) => w.start >= phaseStart && w.start <= phaseEnd);
const q = (xs) => { xs = xs.filter((x) => Number.isFinite(x)).sort((a, b) => a - b); if (!xs.length) return '-'; const at = (f) => xs[Math.min(xs.length - 1, Math.floor(xs.length * f))]; return `n ${xs.length} p25 ${at(0.25)} p50 ${at(0.5)} p90 ${at(0.9)} mean ${Math.round(xs.reduce((a, b) => a + b, 0) / xs.length)}`; };
console.log(`mutant phase ${((phaseEnd - phaseStart) / 1000).toFixed(1)} s; Vitest workers in it ${inPhase.length} (all ${workers.length})`);
const runners = new Map();
for (const w of inPhase) { if (!runners.has(w.ppid)) runners.set(w.ppid, []); runners.get(w.ppid).push(w); }
console.log(`test runners (Vitest instances) forking workers: ${runners.size}`);
console.log(`worker lifetime ms (fork to exit): ${q(inPhase.map((w) => w.exit - w.start))}`);
console.log(`worker boot ms (process start to its first JS): ${q(inPhase.map((w) => w.bootMs))}`);
console.log(`start message to run message ms: ${q(inPhase.map((w) => w.runs[0] && w.startMsg ? w.runs[0].at - w.startMsg : NaN))}`);
console.log(`process start to run message ms: ${q(inPhase.map((w) => w.runs[0] ? w.runs[0].at - w.start : NaN))}`);
console.log(`run message to exit ms: ${q(inPhase.map((w) => w.runs[0] ? w.exit - w.runs[0].at : NaN))}`);
console.log(`files per worker: ${q(inPhase.map((w) => w.runs.reduce((n, r) => n + (r.files?.length ?? 0), 0)))}`);
const gaps = [];
let busy = 0;
for (const list of runners.values()) {
  list.sort((a, b) => a.start - b.start);
  for (let i = 1; i < list.length; i++) gaps.push(list[i].start - list[i - 1].exit);
  for (const w of list) busy += (w.exit ?? phaseEnd) - w.start;
}
console.log(`a runner's next worker after the previous one's exit, ms: ${q(gaps)}`);
console.log(`share of runner time a worker of it was alive: ${(busy / ((phaseEnd - phaseStart) * Math.max(1, runners.size))).toFixed(2)}`);
const files = new Map();
for (const w of inPhase) for (const r of w.runs) for (const f of r.files ?? []) files.set(f, (files.get(f) ?? 0) + 1);
console.log(`distinct test files run ${files.size}; most run: ${[...files].sort((a, b) => b[1] - a[1]).slice(0, 5).map(([f, n]) => `${f} ${n}`).join(', ')}`);
