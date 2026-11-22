// Supercov's strong verdicts for a Rust run against cargo-mutants' on the
// same project: how release validation checks the Rust strong pass.
//
//   node scripts/cargo-mutants-agreement.mjs [--root DIR] [--run ID] [--json]
//
// It reads cargo-mutants' `mutants.out/outcomes.json` (from `cargo mutants`
// run in DIR, one shard or several merged: give `--outcomes FILE` more than
// once) and the run's `strong-mutation.json` (the latest run's where `--run`
// is not given), joins them by mutant id
// (`file:startLine:startCol-endLine:endCol:Genre:replacement`, `removed` for
// an empty replacement), and prints the counts and every disagreement:
//
// - a soundness violation: cargo-mutants caught it (or timed out) and
//   Supercov says survived or no-coverage, or the reverse;
// - viability: one of them calls it unviable and the other does not;
// - only one of them has it.
//
// With `--weak FILE` (the run's `mutants --format jsonl`), it also checks
// the weak verdicts: a mutant cargo-mutants catches that the run calls
// not-infected or not-reached is a weak soundness violation, and the
// statuses are counted.
//
// The exit code is 1 where there is a soundness violation.

import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const option = (name) => {
  const at = args.indexOf(name);
  return at >= 0 ? args[at + 1] : undefined;
};
const root = path.resolve(option("--root") ?? ".");
const outcomesFiles = args
  .flatMap((arg, at) => (arg === "--outcomes" ? [args[at + 1]] : []))
  .map((file) => path.resolve(file));
if (outcomesFiles.length === 0) outcomesFiles.push(path.join(root, "mutants.out/outcomes.json"));

const runsDir = path.join(root, ".supercov/runs");
const run =
  option("--run") ??
  readdirSync(runsDir)
    .filter((name) => statSync(path.join(runsDir, name, "strong-mutation.json"), { throwIfNoEntry: false }))
    .sort((a, b) => statSync(path.join(runsDir, b)).mtimeMs - statSync(path.join(runsDir, a)).mtimeMs)[0];
const supercov = JSON.parse(readFileSync(path.join(runsDir, run, "strong-mutation.json"), "utf8")).mutants;

const reference = new Map();
for (const file of outcomesFiles) {
  for (const outcome of JSON.parse(readFileSync(file, "utf8")).outcomes) {
    const mutant = outcome.scenario?.Mutant;
    if (!mutant) continue;
    const { start, end } = mutant.span;
    const id = `${mutant.file}:${start.line}:${start.column}-${end.line}:${end.column}:${mutant.genre}:${mutant.replacement || "removed"}`;
    reference.set(id, outcome.summary);
  }
}

const detected = (status) => status === "killed" || status === "timeout";
const caught = (summary) => summary === "CaughtMutant" || summary === "Timeout";
const counts = { matched: 0, agree: 0, onlyCargoMutants: [], onlySupercov: [] };
const violations = [];
const viability = [];
for (const [id, summary] of reference) {
  const verdict = supercov[id];
  if (!verdict) {
    counts.onlyCargoMutants.push(id);
    continue;
  }
  counts.matched += 1;
  const unviable = [summary === "Unviable", verdict.status === "unviable"];
  if (unviable[0] || unviable[1]) {
    if (unviable[0] === unviable[1]) counts.agree += 1;
    else viability.push({ id, cargoMutants: summary, supercov: verdict.status, reason: verdict.reason });
    continue;
  }
  if (caught(summary) === detected(verdict.status)) {
    counts.agree += 1;
  } else {
    violations.push({ id, cargoMutants: summary, supercov: verdict.status, reason: verdict.reason ?? null });
  }
}
for (const id of Object.keys(supercov)) if (!reference.has(id)) counts.onlySupercov.push(id);

const weakFile = option("--weak");
const weak = { statuses: {}, violations: [] };
if (weakFile) {
  for (const line of readFileSync(weakFile, "utf8").split("\n")) {
    if (!line.trim()) continue;
    const mutant = JSON.parse(line);
    weak.statuses[mutant.status] = (weak.statuses[mutant.status] ?? 0) + 1;
    const summary = reference.get(mutant.id);
    if (caught(summary) && (mutant.status === "not-infected" || mutant.status === "not-reached")) {
      weak.violations.push({ id: mutant.id, cargoMutants: summary, weak: mutant.status, reason: mutant.weak?.reason ?? null });
    }
  }
}

const report = {
  weak,
  run,
  matched: counts.matched,
  agree: counts.agree,
  soundnessViolations: violations,
  viabilityDisagreements: viability,
  onlyCargoMutants: counts.onlyCargoMutants,
  onlySupercov: counts.onlySupercov,
};
if (args.includes("--json")) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`run ${run}: ${counts.matched} matched, ${counts.agree} agree`);
  console.log(`soundness violations: ${violations.length}`);
  for (const v of violations) console.log(`  ${v.id}: cargo-mutants ${v.cargoMutants}, Supercov ${v.supercov}${v.reason ? ` (${v.reason})` : ""}`);
  console.log(`viability disagreements: ${viability.length}`);
  for (const v of viability) console.log(`  ${v.id}: cargo-mutants ${v.cargoMutants}, Supercov ${v.supercov}${v.reason ? ` (${v.reason})` : ""}`);
  console.log(`only cargo-mutants: ${counts.onlyCargoMutants.length}; only Supercov: ${counts.onlySupercov.length}`);
  if (weakFile) {
    console.log(`weak statuses: ${JSON.stringify(weak.statuses)}`);
    console.log(`weak soundness violations: ${weak.violations.length}`);
    for (const v of weak.violations) console.log(`  ${v.id}: cargo-mutants ${v.cargoMutants}, weak ${v.weak} (${v.reason})`);
  }
  for (const id of counts.onlyCargoMutants.slice(0, 20)) console.log(`  - ${id}`);
  for (const id of counts.onlySupercov.slice(0, 20)) console.log(`  + ${id}`);
}
process.exitCode = violations.length + weak.violations.length > 0 ? 1 : 0;
