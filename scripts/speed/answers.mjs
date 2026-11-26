// Every answer a run gives, normalised so that two runs of the same suite
// compare equal unless what they measured differs:
//
//   node scripts/speed/answers.mjs <supercov binary> <project> <run id> <out.json> [--quick]
//
// The run's summary and every view of it, every file, every line of every
// file (with its tests and phases), every decision (MC/DC vectors and
// witness pairs; left out with --quick, for a large run), every test, the
// mutants and their candidates, and every record of its evidence. Random ids
// (run, attempt, worker, test key), times, process ids and paths are
// replaced by stable names, and lists are sorted after that. A change meant
// not to change what Supercov measures leaves two runs' files equal byte for
// byte (docs/performance.md, "Prove a speed-up changes nothing").
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

const [binary, project, run, out] = process.argv.slice(2);
const quick = process.argv.includes('--quick');
const runDir = join(project, '.supercov/runs', run);

function query(args) {
  const result = spawnSync(binary, ['runs', run, ...args, '--json'], { cwd: project, encoding: 'utf8', maxBuffer: 1 << 30 });
  try {
    return JSON.parse(result.stdout);
  } catch {
    return { failed: args.join(' '), status: result.status, stderr: result.stderr.slice(0, 500) };
  }
}

// Evidence archive: SUPERCOV-EVIDENCE-3, then entries of a 4-byte length, a
// JSON header naming `path` and `bytes`, and the bytes.
function archive() {
  const data = gunzipSync(readFileSync(join(runDir, 'evidence.raw.gz')));
  let at = data.indexOf(10) + 1;
  const entries = [];
  while (at < data.length) {
    const length = data.readUInt32BE(at);
    at += 4;
    const header = JSON.parse(data.subarray(at, at + length).toString());
    at += length;
    entries.push({ path: header.path, text: data.subarray(at, at + header.bytes).toString() });
    at += header.bytes;
  }
  return entries;
}

const entries = archive();
// Stable names for the random ids: an attempt by its test and retry, a worker
// by its order, the run as RUN.
const names = new Map();
names.set(run, 'RUN');
for (const { text } of entries)
  for (const match of text.matchAll(/"testId":"([^"]+)","testKey":"([0-9a-f]+)","retry":(\d+),"attemptId":"([0-9a-f]+)"/g)) {
    names.set(match[4], `ATTEMPT(${match[1]}#${match[3]})`);
    names.set(match[2], `KEY(${match[1]})`);
  }
for (const { text } of entries)
  for (const match of text.matchAll(/"workerId":"([^"]+)"/g))
    if (!names.has(match[1])) names.set(match[1], `WORKER${names.size}`);
const workspace = /\/\.supercov\/workspaces\/[^"\s]*/g;

function scrub(value, key) {
  if (typeof value === 'string') {
    let text = value.replace(workspace, 'WORKSPACE').split(project).join('PROJECT');
    for (const [from, to] of names) if (text.includes(from)) text = text.split(from).join(to);
    text = text.replace(/run_[0-9a-f]{16}/g, 'RUN').replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z/g, 'TIME');
    text = text.replace(/pid-\d+-[0-9a-z]+/g, 'PID');
    return text;
  }
  if (Array.isArray(value)) {
    const items = value.map((item) => scrub(item, key));
    return items.map((item) => [JSON.stringify(item), item]).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, item]) => item);
  }
  if (value && typeof value === 'object' && value.eventColumns && Array.isArray(value.eventColumns.id)) {
    // Columns as one row per event, so sorting keeps each event whole: a
    // hit's id by name, its phase and kind by name, its time left out.
    const columns = value.eventColumns;
    const { eventColumns: _columns, ...rest } = value;
    const rows = columns.id.map((id, position) => ({
      id: typeof id === 'number' ? value.hits[id] : id,
      type: columns.types[columns.type[position]],
      phase: columns.phase[position] === -1 ? null : columns.phases[columns.phase[position]],
      environment: columns.environments[columns.environment[position]],
      vector: columns.vector?.[position],
    }));
    return scrub({ ...rest, events: rows }, key);
  }
  if (value && typeof value === 'object') {
    const object = {};
    for (const name of Object.keys(value).sort()) {
      if (/^(timestampMs|startedAtMs|endedAtMs|durationMs|at|t|pid|ppid|generatedAt|createdAt|duration|elapsedMs|timings|span|sha256|bytes|size|mtime.*|.*Ms)$/.test(name)) continue;
      object[scrub(name)] = scrub(value[name], name);
    }
    return object;
  }
  return value;
}

const answers = {};
// The mutants first: asking for them stores their summary, which the run's
// summary then shows.
answers.mutants = query(['mutants', '--limit', '0']);
const summary = query([]);
answers.summary = summary;
for (const view of ['files', 'gaps', 'kinds', 'runners', 'scope']) answers[view] = query([view, '--limit', '0']);
answers.minimize = query(['minimize']);
answers.withoutEvidence = query(['tests', 'without-evidence', '--limit', '0']);
const files = (answers.files.data?.files ?? []).map((file) => file.file);
answers.file = {};
answers.line = {};
answers.decision = {};
// Every line of every file: one query each where the run is small (it gives
// the line's phases too), else all of a file's lines in one, and each
// decision of the manifest on its own.
const manifest = entries.find((entry) => entry.path === 'manifest.json');
const decisionLines = new Set((manifest ? JSON.parse(manifest.text).decisions ?? [] : []).map((decision) => `${decision.file}:${decision.line}`));
const sizes = files.map((file) => (existsSync(join(project, file)) ? readFileSync(join(project, file), 'utf8').split('\n').length : 0));
const single = sizes.reduce((sum, lines) => sum + lines, 0) <= 3000;
const list = join(tmpdir(), `answers-${process.pid}.txt`);
files.forEach((file, index) => {
  answers.file[file] = query(['file', file, '--limit', '0']);
  const locations = Array.from({ length: sizes[index] }, (_, line) => `${file}:${line + 1}`);
  if (single) for (const location of locations) answers.line[location] = query(['line', location, '--limit', '0']);
  else if (locations.length > 0) {
    writeFileSync(list, `${locations.join('\n')}\n`);
    for (const answer of query(['line', '--from-file', list, '--limit', '0']).data?.lines ?? [])
      answers.line[`${answer.location.file}:${answer.location.line}`] = answer;
  }
});
if (!quick) for (const location of [...decisionLines].sort()) answers.decision[location] = query(['decision', location, '--limit', '0']);
// Every test the run holds, by id.
const tests = new Set();
for (const answer of Object.values(answers.line))
  for (const test of (answer.data ?? answer).tests ?? []) tests.add(test.id);
for (const execution of JSON.parse(readFileSync(join(runDir, 'test-executions.json'), 'utf8')).executions ?? [])
  if (execution.testId) tests.add(execution.testId);
answers.test = {};
// A large suite's tests are each in the evidence and in its lines' answers:
// asked one by one only where there are a few hundred.
if (!quick || tests.size <= 300)
  for (const test of [...tests].sort()) answers.test[test] = query(['test', test, '--limit', '0']);

// The evidence, entry by entry: a JSON Lines entry as its set of records.
answers.evidence = {};
for (const { path, text } of entries) {
  // Entries named by a process or a random token are one group: their
  // records together, whoever wrote them.
  const name = scrub(path)
    .replace(/^execution\.host\..*\.jsonl$/, 'execution.host.jsonl')
    .replace(/^server\/background\/process-.*\.jsonl$/, 'server/background/process.jsonl')
    .replace(/^playwright-(worker|status)-.*\.mcdc\.jsonl$/, 'playwright-$1.mcdc.jsonl');
  if (/\.jsonl$/.test(path)) (answers.evidence[name] ??= []).push(...text.split('\n').filter(Boolean).map((line) => JSON.parse(line)));
  else (answers.evidence[name] ??= []).push(/\.json$/.test(path) ? JSON.parse(text) : text);
}
writeFileSync(out, `${JSON.stringify(scrub(answers), null, 1)}\n`);
console.log(`${out}: ${files.length} files, ${Object.keys(answers.line).length} lines, ${Object.keys(answers.decision).length} decisions, ${tests.size} tests, ${entries.length} evidence entries`);
