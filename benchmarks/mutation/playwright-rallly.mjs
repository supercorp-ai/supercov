// A real Playwright suite's strong pass: Rallly (Next.js production build,
// Postgres, Mailpit), a few of its spec files, measured and then a strong
// pass bounded by a budget, the pass's directory kept for
// playwright-breakdown.mjs to say where its time went.
//
//   node playwright-rallly.mjs --supercov <binary> --out <dir> [--work <dir>] [--budget 300] [--specs a.spec.ts,b.spec.ts]
//
// Needs Postgres on localhost:5450 (database `rallly`) and Mailpit on
// localhost:1025/8025, as Rallly's .env.test says.
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, word, at, all) =>
    word.startsWith("--") ? [...pairs, [word.slice(2), all[at + 1]]] : pairs, []));
const supercov = resolve(args.supercov);
const out = resolve(args.out ?? "out");
const work = resolve(args.work ?? "work");
const budget = args.budget ?? "300";
const specs = (args.specs ?? "tests/vote-and-comment.spec.ts,tests/create-delete-poll.spec.ts").split(",");
mkdirSync(out, { recursive: true });
mkdirSync(work, { recursive: true });

const seconds = {};
function run(name, command, options = {}) {
    const started = Date.now();
    const result = spawnSync(command[0], command.slice(1), { encoding: "utf8", maxBuffer: 1 << 30, ...options, env: { ...process.env, CI: "true", ...options.env } });
    seconds[name] = (Date.now() - started) / 1000;
    writeFileSync(join(out, `${name}.log`), `$ ${command.join(" ")}\n${result.stdout ?? ""}\n${result.stderr ?? ""}\nexit ${result.status}\n`);
    console.log(`[rallly] ${name}: exit ${result.status} in ${seconds[name].toFixed(1)} s`);
    if (result.status !== 0 && !options.mayFail)
        throw new Error(`${name} failed: see ${name}.log`);
    return result;
}

const root = join(work, "rallly");
if (!existsSync(root)) {
    mkdirSync(root, { recursive: true });
    run("clone-init", ["git", "init", "-q"], { cwd: root });
    run("clone-fetch", ["git", "fetch", "-q", "--depth", "1", "https://github.com/lukevella/rallly.git", "26111d7e993d2e7016810f68bcad139393644db0"], { cwd: root });
    run("clone-checkout", ["git", "checkout", "-q", "FETCH_HEAD"], { cwd: root });
}
const project = join(root, "apps/web");
const pnpm = ["npx", "-y", "pnpm@12.14.0"];
run("install", [...pnpm, "install", "--frozen-lockfile"], { cwd: root });
run("db-generate", [...pnpm, "db:generate"], { cwd: root });
// The suite's own configuration, with the build in the command (Supercov
// measures the server it builds) and the one worker it is written for.
writeFileSync(join(project, "playwright.bench.config.ts"), `import base from './playwright.config';

export default {
  ...base,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  webServer: {
    ...base.webServer,
    command: 'NODE_ENV=test npx next build && npx next start --port 3002',
    timeout: 900_000,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 10_000 },
  },
};
`);
run("browser", ["npx", "playwright", "install", "chromium"], { cwd: project });
const database = "postgres://postgres:postgres@localhost:5450/rallly";
function reset(label) {
    run(`${label}-db-reset`, [...pnpm, "--filter", "@rallly/database", "exec", "prisma", "migrate", "reset", "--force"], { cwd: root, env: { DATABASE_URL: database, DIRECT_DATABASE_URL: database } });
    run(`${label}-mail-reset`, ["curl", "-fsS", "-X", "DELETE", "http://localhost:8025/api/v1/messages"], { cwd: root });
}
const command = ["npx", "playwright", "test", "--config", "playwright.bench.config.ts", ...specs];
reset("measure");
run("supercov-measure", [supercov, "--", ...command], { cwd: project, env: { SUPERCOV_MUTANTS: "1" }, mayFail: true });
reset("strong");
const strong = run("supercov-strong", [supercov, "runs", "latest", "mutants", "test", "--budget", budget, "--json"], { cwd: project, env: { SUPERCOV_STRONG_KEEP: "1" }, mayFail: true });
run("supercov-export", [supercov, "runs", "latest", "mutants", "--format", "jsonl", "--output", join(out, "supercov-mutants.jsonl")], { cwd: project, mayFail: true });
let pass = {};
try {
    pass = JSON.parse(strong.stdout);
}
catch {
    // The log has it.
}
const strongDirectory = join(project, ".supercov/strong");
if (existsSync(strongDirectory))
    cpSync(strongDirectory, join(out, "strong"), { recursive: true });
// Every log the pass and its commands wrote (a round's command output, the
// server's build), wherever under .supercov it is: a round that fails says
// why only there.
const logs = join(out, "supercov-logs");
const collect = (directory, depth = 0) => {
    let entries;
    try {
        entries = readdirSync(directory, { withFileTypes: true });
    }
    catch {
        return;
    }
    for (const entry of entries) {
        const path = join(directory, entry.name);
        if (entry.isDirectory() && entry.name !== "node_modules" && entry.name !== ".next" && depth < 12)
            collect(path, depth + 1);
        else if (entry.isFile() && /\.log$/.test(entry.name)) {
            const target = join(logs, path.slice(project.length + 1).replaceAll("/", "__"));
            mkdirSync(logs, { recursive: true });
            cpSync(path, target);
        }
    }
};
collect(join(project, ".supercov"));
writeFileSync(join(out, "summary.json"), JSON.stringify({ project: "rallly", specs, budget: Number(budget), seconds: { ...seconds, supercovStrong: seconds["supercov-strong"] }, supercovSummary: pass.summary ?? null, driver: pass.passes ?? [] }, null, 2));
console.log(JSON.stringify({ seconds, summary: pass.summary, passes: pass.passes }, null, 1));
