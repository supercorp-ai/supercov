// Where a Vitest Browser Mode job's time goes, on a project run.mjs set up:
// the whole command, Vitest's own phases, a Chromium launch, and a fresh
// context and page in a running Chromium.
//
//   node diag-browser.mjs <project dir> [runs]
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { join } from "node:path";

const directory = process.argv[2];
const runs = Number(process.argv[3] ?? 5);
const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];

const whole = [];
for (let run = 0; run < runs; run += 1) {
    const started = process.hrtime.bigint();
    const result = spawnSync("npx", ["vitest", "run"], { cwd: directory, encoding: "utf8", env: { ...process.env, CI: "true" } });
    whole.push(Number(process.hrtime.bigint() - started) / 1e6);
    const duration = `${result.stdout}${result.stderr}`.split("\n").find((line) => line.includes("Duration"));
    console.log(`vitest run ${run}: ${whole.at(-1).toFixed(0)} ms, exit ${result.status}; ${duration?.trim() ?? "(no Duration line)"}`);
}
console.log(`vitest run, median: ${median(whole).toFixed(0)} ms`);

const require = createRequire(join(directory, "package.json"));
const { chromium } = require("playwright");
const launches = [];
const contexts = [];
for (let run = 0; run < runs; run += 1) {
    let started = process.hrtime.bigint();
    const browser = await chromium.launch({ headless: true });
    launches.push(Number(process.hrtime.bigint() - started) / 1e6);
    for (let page = 0; page < 3; page += 1) {
        started = process.hrtime.bigint();
        const context = await browser.newContext();
        const opened = await context.newPage();
        await opened.goto("about:blank");
        contexts.push(Number(process.hrtime.bigint() - started) / 1e6);
        await context.close();
    }
    await browser.close();
}
console.log(`chromium launch, median: ${median(launches).toFixed(0)} ms (${launches.map((ms) => ms.toFixed(0)).join(", ")})`);
console.log(`new context + page, median: ${median(contexts).toFixed(0)} ms`);

const started = process.hrtime.bigint();
const node = spawnSync(process.execPath, ["-e", "0"]);
console.log(`node start: ${(Number(process.hrtime.bigint() - started) / 1e6).toFixed(0)} ms, exit ${node.status}`);
