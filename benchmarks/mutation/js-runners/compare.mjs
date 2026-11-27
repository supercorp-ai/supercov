// Old against new: each mutant's strong verdict from two runs of run.mjs on the
// same project (two Supercov binaries), and the pass's times.
//
//   node compare.mjs <old out dir> <new out dir> [--markdown]
//
// A verdict may only move from undecided (no verdict, `error`) to decided.
// Anything else is a violation: a kill turned survivor or the reverse, a
// survivor turned timeout or kill, a kill or timeout dropped. Exits 1 on any.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const [oldDir, newDir] = process.argv.slice(2).filter((word) => !word.startsWith("--"));
const markdown = process.argv.includes("--markdown");

function verdicts(directory) {
    const path = join(directory, "supercov-mutants.jsonl");
    const map = new Map();
    if (!existsSync(path))
        return map;
    for (const line of readFileSync(path, "utf8").split("\n")) {
        if (!line.trim())
            continue;
        let mutant;
        try {
            mutant = JSON.parse(line);
        }
        catch {
            continue;
        }
        if (typeof mutant?.id !== "string")
            continue;
        map.set(mutant.id, mutant.strong?.status ?? (mutant.status === "equivalent" ? "equivalent" : "none"));
    }
    return map;
}

function summary(directory) {
    try {
        return JSON.parse(readFileSync(join(directory, "summary.json"), "utf8"));
    }
    catch {
        return {};
    }
}

const undecided = (status) => status === "none" || status === "error" || status === undefined;
const detected = (status) => status === "killed" || status === "timeout";

const before = verdicts(oldDir);
const after = verdicts(newDir);
const counts = new Map();
const violations = [];
for (const id of new Set([...before.keys(), ...after.keys()])) {
    const was = before.get(id);
    const is = after.get(id);
    const key = `${was ?? "absent"} -> ${is ?? "absent"}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
    if (was === is || undecided(was))
        continue;
    // A decided verdict that changed, or went undecided.
    if (undecided(is) || detected(was) !== detected(is))
        violations.push({ id, was, is });
}
// killed <-> timeout is the one change between decided verdicts that is not a
// flip of detection; it is still reported.
const kindChanges = [...before].filter(([id, was]) => detected(was) && detected(after.get(id)) && was !== after.get(id)).length;

const oldSummary = summary(oldDir);
const newSummary = summary(newDir);
const seconds = (one) => one.seconds?.supercovStrong;
const driver = (one) => (one.driver ?? []).map((pass) => pass.summary ?? pass).find((entry) => entry?.driver === "jobs") ?? null;
const lines = [];
const project = newSummary.project ?? oldSummary.project ?? "?";
const ran = newSummary.supercovSummary?.ran ?? oldSummary.supercovSummary?.ran;
lines.push(`### ${project}`);
lines.push("");
lines.push(`| | old | new |`);
lines.push(`|---|---:|---:|`);
lines.push(`| strong pass, s | ${seconds(oldSummary)?.toFixed(1) ?? "-"} | ${seconds(newSummary)?.toFixed(1) ?? "-"} |`);
if (ran)
    lines.push(`| ms per mutant run (${ran}) | ${Math.round(seconds(oldSummary) * 1000 / ran)} | ${Math.round(seconds(newSummary) * 1000 / ran)} |`);
for (const field of ["jobs", "baselines", "confirmations", "stopped", "alone", "jobMs", "resident", "residentJobs"]) {
    const was = driver(oldSummary)?.[field];
    const is = driver(newSummary)?.[field];
    if (was !== undefined || is !== undefined)
        lines.push(`| driver ${field} | ${was ?? "-"} | ${is ?? "-"} |`);
}
lines.push("");
lines.push(`Verdicts (old -> new): ${[...counts].sort().map(([key, count]) => `${key}: ${count}`).join("; ")}`);
lines.push("");
lines.push(`Violations: ${violations.length}${kindChanges ? `; killed<->timeout: ${kindChanges}` : ""}`);
for (const violation of violations.slice(0, 40))
    lines.push(`- ${violation.id}: ${violation.was} -> ${violation.is}`);
lines.push("");
console.log(markdown ? lines.join("\n") : JSON.stringify({ project, counts: Object.fromEntries(counts), violations, kindChanges, old: seconds(oldSummary), new: seconds(newSummary) }, null, 1));
process.exitCode = violations.length ? 1 : 0;
