// Old against new: each mutant's strong verdict from runs of run.mjs on the
// same project under two Supercov binaries, and the pass's times.
//
//   node compare.mjs <old out dir>[,<old out dir>...] <new out dir>[,...] [--markdown]
//
// The k-th old run is compared with the k-th new run (runs taken in turn on
// one machine), and the old runs with one another: what moves with no change
// at all. A verdict may only move from undecided (no verdict, `error`) to
// decided. Anything else between old and new is a violation: a kill turned
// survivor or the reverse, a survivor turned timeout or kill, a kill or
// timeout dropped. Exits 1 on any. Killed <-> timeout is counted apart.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const [oldDirs, newDirs] = process.argv.slice(2).filter((word) => !word.startsWith("--")).map((list) => list.split(","));
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

function diff(before, after) {
    const counts = new Map();
    const violations = [];
    let kindChanges = 0;
    for (const id of new Set([...before.keys(), ...after.keys()])) {
        const was = before.get(id);
        const is = after.get(id);
        const key = `${was ?? "absent"} -> ${is ?? "absent"}`;
        counts.set(key, (counts.get(key) ?? 0) + 1);
        if (was === is || undecided(was))
            continue;
        if (undecided(is) || detected(was) !== detected(is))
            violations.push({ id, was, is });
        else
            kindChanges += 1;
    }
    return { counts, violations, kindChanges };
}

const olds = oldDirs.map((directory) => ({ verdicts: verdicts(directory), summary: summary(directory) }));
const news = (newDirs ?? []).map((directory) => ({ verdicts: verdicts(directory), summary: summary(directory) }));
const seconds = (run) => run.summary.seconds?.supercovStrong;
// The jobs driver's summary, or the Vitest pass's (its counts in `profile`).
const driver = (run) => {
    const entries = run.summary.driver ?? [];
    const jobs = entries.find((entry) => entry?.driver === "jobs");
    if (jobs)
        return jobs;
    const first = entries[0];
    return first ? { ...first, ...(first.profile ?? {}) } : null;
};
const mean = (values) => (values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : undefined);
const project = news[0]?.summary.project ?? olds[0]?.summary.project ?? "?";
const ran = news[0]?.summary.supercovSummary?.ran ?? olds[0]?.summary.supercovSummary?.ran;

const lines = [`### ${project}`, "", "| | old | new |", "|---|---:|---:|"];
const times = (runs) => runs.map(seconds).filter(Number.isFinite);
lines.push(`| strong pass, s (each run) | ${times(olds).map((value) => value.toFixed(1)).join(", ")} | ${times(news).map((value) => value.toFixed(1)).join(", ")} |`);
if (ran)
    lines.push(`| ms per mutant run (${ran}), mean | ${Math.round(mean(times(olds)) * 1000 / ran)} | ${Math.round(mean(times(news)) * 1000 / ran)} |`);
for (const field of ["jobs", "copies", "baselines", "confirmations", "stopped", "alone", "jobMs", "residentJobs", "residentFallbacks"]) {
    const was = olds.map((run) => driver(run)?.[field]).filter((value) => value !== undefined);
    const is = news.map((run) => driver(run)?.[field]).filter((value) => value !== undefined);
    if (was.length || is.length)
        lines.push(`| driver ${field} | ${was.join(", ") || "-"} | ${is.join(", ") || "-"} |`);
}
// Copies (files) a decided mutant ran, by its verdict (the Vitest pass).
for (const status of ["killed", "timeout", "survived"]) {
    const perMutant = (run) => { const entry = driver(run)?.filesByStatus?.[status]; return entry?.mutants ? `${entry.files} for ${entry.mutants} (${(entry.files / entry.mutants).toFixed(2)})` : undefined; };
    const was = olds.map(perMutant).filter(Boolean);
    const is = news.map(perMutant).filter(Boolean);
    if (was.length || is.length)
        lines.push(`| files run by ${status} mutants | ${was.join("; ") || "-"} | ${is.join("; ") || "-"} |`);
}
const jobMs = (runs) => mean(runs.map((run) => { const one = driver(run); return one?.jobs ? one.jobMs / one.jobs : undefined; }).filter(Number.isFinite));
if (jobMs(olds) !== undefined || jobMs(news) !== undefined)
    lines.push(`| ms of a worker per job, mean | ${Math.round(jobMs(olds) ?? NaN)} | ${Math.round(jobMs(news) ?? NaN)} |`);
lines.push("");
let violations = 0;
for (let at = 0; at < Math.max(olds.length, news.length); at += 1) {
    if (!olds[at] || !news[at])
        continue;
    const { counts, violations: found, kindChanges } = diff(olds[at].verdicts, news[at].verdicts);
    violations += found.length;
    lines.push(`Run ${at + 1}, old -> new: ${[...counts].sort().map(([key, count]) => `${key}: ${count}`).join("; ")}`);
    lines.push(`Violations: ${found.length}${kindChanges ? `; killed<->timeout: ${kindChanges}` : ""}`);
    for (const violation of found.slice(0, 40))
        lines.push(`- ${violation.id}: ${violation.was} -> ${violation.is}`);
    lines.push("");
}
for (let at = 1; at < olds.length; at += 1) {
    const { violations: moved, kindChanges } = diff(olds[0].verdicts, olds[at].verdicts);
    lines.push(`Old run 1 against old run ${at + 1} (no change): ${moved.length} moved${kindChanges ? `, killed<->timeout: ${kindChanges}` : ""}`);
    for (const one of moved.slice(0, 20))
        lines.push(`- ${one.id}: ${one.was} -> ${one.is}`);
    lines.push("");
}
console.log(markdown ? lines.join("\n") : JSON.stringify({ project, violations, lines }, null, 1));
process.exitCode = violations ? 1 : 0;
