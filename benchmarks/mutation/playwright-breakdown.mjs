// Where a kept Playwright strong pass spent its time (SUPERCOV_STRONG_KEEP=1,
// the pass's `.supercov/strong` directory): each attempt's own time by kind
// (a mutant's, a baseline's, a control's), the time between one attempt's
// end and the next one's start in the same worker (Playwright's test start
// and end, its fixtures, copies that took nothing, a new worker), and the
// time before each invocation's first attempt (the server's build and start).
//
//   node playwright-breakdown.mjs <strong dir> [--markdown]
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const directory = process.argv[2];
const markdown = process.argv.includes("--markdown");

function* attemptFiles(at, depth = 0) {
    let entries;
    try {
        entries = readdirSync(at, { withFileTypes: true });
    }
    catch {
        return;
    }
    for (const entry of entries) {
        const path = join(at, entry.name);
        if (entry.isDirectory() && depth < 8)
            yield* attemptFiles(path, depth + 1);
        else if (entry.isFile() && /[\\/]attempts$/.test(at) && entry.name.endsWith(".json"))
            yield path;
    }
}

const attempts = [];
for (const path of attemptFiles(directory)) {
    try {
        const attempt = JSON.parse(readFileSync(path, "utf8"));
        if (Number.isFinite(attempt.startedAt))
            attempts.push(attempt);
    }
    catch {
        // A file being written.
    }
}
const sum = (values) => values.reduce((total, value) => total + value, 0);
const byKind = {};
for (const attempt of attempts) {
    const kind = attempt.kind ?? "?";
    const outcome = attempt.outcome ?? (attempt.endedAt ? "ended" : "unended");
    const entry = (byKind[`${kind} ${outcome}`] ??= { count: 0, ms: 0 });
    entry.count += 1;
    entry.ms += (attempt.endedAt ?? attempt.startedAt) - attempt.startedAt;
}
// Per worker, in order: the gaps between attempts.
const workers = new Map();
for (const attempt of attempts) {
    const key = `${attempt.invocation}:${attempt.worker}`;
    if (!workers.has(key))
        workers.set(key, []);
    workers.get(key).push(attempt);
}
let gapMs = 0;
let gaps = 0;
for (const list of workers.values()) {
    list.sort((a, b) => a.startedAt - b.startedAt);
    for (let at = 1; at < list.length; at += 1) {
        if (!list[at - 1].endedAt)
            continue;
        gapMs += Math.max(0, list[at].startedAt - list[at - 1].endedAt);
        gaps += 1;
    }
}
// Between workers of one invocation: a new worker's start.
const invocations = new Map();
for (const attempt of attempts) {
    const list = invocations.get(attempt.invocation) ?? [];
    list.push(attempt);
    invocations.set(attempt.invocation, list);
}
let workerSwitchMs = 0;
let workerSwitches = 0;
for (const list of invocations.values()) {
    list.sort((a, b) => a.startedAt - b.startedAt);
    for (let at = 1; at < list.length; at += 1) {
        if (list[at].worker !== list[at - 1].worker && list[at - 1].endedAt) {
            workerSwitchMs += Math.max(0, list[at].startedAt - list[at - 1].endedAt);
            workerSwitches += 1;
        }
    }
}
const first = Math.min(...attempts.map((attempt) => attempt.startedAt));
const last = Math.max(...attempts.map((attempt) => attempt.endedAt ?? attempt.startedAt));
const attemptMs = sum(Object.values(byKind).map((entry) => entry.ms));
const lines = [
    `attempts: ${attempts.length} in ${invocations.size} invocation(s), ${workers.size} worker process(es); first to last ${((last - first) / 1000).toFixed(1)} s`,
    ...Object.entries(byKind).sort().map(([kind, entry]) => `  ${kind}: ${entry.count}, ${(entry.ms / 1000).toFixed(1)} s (${Math.round(entry.ms / entry.count)} ms each)`),
    `between attempts in one worker: ${gaps} gaps, ${(gapMs / 1000).toFixed(1)} s (${Math.round(gapMs / Math.max(1, gaps))} ms each)`,
    `new workers within an invocation: ${workerSwitches}, ${(workerSwitchMs / 1000).toFixed(1)} s`,
    `attempts' own time: ${(attemptMs / 1000).toFixed(1)} s of ${((last - first) / 1000).toFixed(1)} s`,
];
console.log(markdown ? `\`\`\`\n${lines.join("\n")}\n\`\`\`` : lines.join("\n"));
