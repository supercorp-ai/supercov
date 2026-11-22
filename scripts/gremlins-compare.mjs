#!/usr/bin/env node
// Compares a Go strong pass's verdicts with Gremlins' on the same module.
//
//   node scripts/gremlins-compare.mjs <module dir> <gremlins report.json> [--json]
//
// Gremlins is run with every test per mutant and the mutators Supercov
// lists:
//
//   gremlins unleash --integration --coverpkg ./... --invert-assignments \
//     --invert-logical --invert-loopctrl --remove-self-assignments -o report.json .
//
// Supercov's verdicts are read from the module's latest run
// (`.supercov/runs/<id>/strong-mutation.json`). A mutant is joined by its
// file, its token's line and column, and its Gremlins mutator.
//
// A disagreement is listed with its kind:
//
//   soundness   Gremlins killed (or timed out) and Supercov says survived or
//               no-coverage, or the reverse: Gremlins says LIVED and
//               Supercov killed it.
//   unviable    Supercov says the mutant does not compile; Gremlins counts a
//               build that fails (exit 1) as KILLED.
//   uncovered   Gremlins says NOT COVERED (its coverage profile has no block
//               there, as for a constant declaration) and Supercov ran it.
//   other       anything else (error, timeout against kill).
//
// Exit 1 when there is a soundness disagreement.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const [moduleArgument, reportArgument, ...flags] = process.argv.slice(2);
if (!moduleArgument || !reportArgument) {
  console.error("usage: gremlins-compare.mjs <module dir> <gremlins report.json> [--json]");
  process.exit(2);
}
const module = resolve(moduleArgument);
const runs = join(module, ".supercov/runs");
const latest = readdirSync(runs)
  .filter((name) => name.startsWith("run_"))
  .map((name) => ({ name, at: statSync(join(runs, name)).mtimeMs }))
  .filter(({ name }) => {
    try {
      statSync(join(runs, name, "strong-mutation.json"));
      return true;
    } catch {
      return false;
    }
  })
  .sort((a, b) => b.at - a.at)[0];
if (!latest) {
  console.error(`no run with strong verdicts under ${runs}`);
  process.exit(2);
}
const verdicts = JSON.parse(readFileSync(join(runs, latest.name, "strong-mutation.json"), "utf8")).mutants;

const sources = new Map();
function tokenAt(file, line, column) {
  if (!sources.has(file)) sources.set(file, readFileSync(join(module, file), "utf8").split("\n"));
  const text = Buffer.from(sources.get(file)[line - 1] ?? "", "utf8");
  return text.subarray(column - 1).toString("utf8").match(/^(<=|>=|==|!=|<|>)/)?.[1];
}

const boundary = new Set(["<:<=", "<=:<", ">:>=", ">=:>"]);
function gremlinsType(file, line, column, mutator, replacement) {
  switch (mutator) {
    case "ArithmeticOperator":
    case "UnaryOperator":
      return "ARITHMETIC_BASE";
    case "AssignmentOperator":
      return replacement === "=" ? "REMOVE_SELF_ASSIGNMENTS" : "INVERT_ASSIGNMENTS";
    case "EqualityOperator":
      return boundary.has(`${tokenAt(file, line, column)}:${replacement}`)
        ? "CONDITIONALS_BOUNDARY"
        : "CONDITIONALS_NEGATION";
    case "LogicalOperator":
      return "INVERT_LOGICAL";
    case "UpdateOperator":
      return "INCREMENT_DECREMENT";
    default:
      return mutator;
  }
}

const ours = new Map();
for (const [id, verdict] of Object.entries(verdicts)) {
  const match = id.match(/^(.*):(\d+):(\d+)-(\d+):(\d+):([A-Za-z_]+):(.*)$/);
  if (!match) continue;
  const [, file, line, column, , , mutator, replacement] = match;
  const key = `${file}:${line}:${column} ${gremlinsType(file, +line, +column, mutator, replacement)}`;
  ours.set(key, { id, ...verdict });
}

const report = JSON.parse(readFileSync(reportArgument, "utf8"));
const theirs = new Map();
for (const file of report.files) {
  for (const mutation of file.mutations) {
    theirs.set(`${file.file_name}:${mutation.line}:${mutation.column} ${mutation.type}`, mutation.status);
  }
}

const detected = (status) => status === "killed" || status === "timeout";
const counts = {};
const disagreements = [];
let matched = 0;
for (const [key, status] of theirs) {
  const verdict = ours.get(key);
  if (!verdict) {
    disagreements.push({ key, kind: "only-gremlins", gremlins: status });
    continue;
  }
  matched += 1;
  const pair = `${status} / ${verdict.status}`;
  counts[pair] = (counts[pair] ?? 0) + 1;
  const gremlinsDetected = status === "KILLED" || status === "TIMED OUT";
  let kind = null;
  if (verdict.status === "unviable") {
    if (status !== "NOT VIABLE") kind = "unviable";
  } else if (status === "NOT COVERED") {
    if (verdict.status !== "no-coverage" && verdict.status !== "survived") kind = "uncovered";
  } else if (gremlinsDetected && !detected(verdict.status)) {
    kind = verdict.status === "error" ? "other" : "soundness";
  } else if (status === "LIVED" && detected(verdict.status)) {
    kind = "soundness";
  } else if (status === "NOT VIABLE") {
    kind = "other";
  }
  if (kind) {
    disagreements.push({
      key,
      id: verdict.id,
      kind,
      gremlins: status,
      supercov: verdict.status,
      killedBy: verdict.killedBy ?? null,
      reason: verdict.reason ?? null,
    });
  }
}
for (const key of ours.keys()) {
  if (!theirs.has(key)) disagreements.push({ key, kind: "only-supercov", supercov: ours.get(key).status });
}
const byKind = {};
for (const { kind } of disagreements) byKind[kind] = (byKind[kind] ?? 0) + 1;
const result = { run: latest.name, matched, counts, disagreements: byKind, list: disagreements };
if (flags.includes("--json")) {
  console.log(JSON.stringify(result, null, 2));
} else {
  console.log(`run ${latest.name}: ${matched} mutants joined`);
  for (const [pair, count] of Object.entries(counts).sort()) console.log(`  ${pair.padEnd(28)} ${count}`);
  console.log("disagreements:", JSON.stringify(byKind));
  for (const entry of disagreements.filter((entry) => entry.kind === "soundness" || entry.kind === "other")) {
    console.log(`  ${entry.kind}: ${entry.key}  gremlins ${entry.gremlins}  supercov ${entry.supercov}${entry.killedBy ? ` by ${entry.killedBy}` : ""}${entry.reason ? ` (${entry.reason})` : ""}`);
  }
}
process.exit(disagreements.some((entry) => entry.kind === "soundness") ? 1 : 0);
