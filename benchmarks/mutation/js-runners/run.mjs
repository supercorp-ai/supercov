// One project of corpus.json: StrykerJS 10.1.1 with every killer recorded
// (disableBail), then Supercov (measure, then `mutants test`), each timed, and
// every mutant's two verdicts joined.
//
//   node run.mjs --project <name> --supercov <binary> --out <dir> [--work <dir>]
//
// Writes <out>/summary.json (times, counts, disagreements), StrykerJS's report,
// Supercov's mutants (jsonl) and every log. A disagreement is a mutant one tool
// detects (killed or timeout) and the other does not.
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { availableParallelism } from "node:os";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { joinMutants, originalCode, readSupercovMutants } from "../supercov-join.mjs";

const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, word, at, all) =>
    word.startsWith("--") ? [...pairs, [word.slice(2), all[at + 1]]] : pairs, []));
const corpus = JSON.parse(readFileSync(new URL("./corpus.json", import.meta.url), "utf8"));
const project = corpus.projects.find((entry) => entry.name === args.project);
if (!project)
    throw new Error(`no project ${args.project} in corpus.json`);
const out = resolve(args.out ?? "out");
const work = resolve(args.work ?? "work");
const supercov = resolve(args.supercov);
mkdirSync(out, { recursive: true });
mkdirSync(work, { recursive: true });

function run(name, command, options = {}) {
    const started = Date.now();
    const result = spawnSync(command[0], command.slice(1), { encoding: "utf8", maxBuffer: 1 << 30, ...options, env: { ...process.env, ...project.env, ...options.env } });
    const seconds = (Date.now() - started) / 1000;
    writeFileSync(join(out, `${name}.log`), `$ ${command.join(" ")}\n${result.stdout ?? ""}\n${result.stderr ?? ""}\nexit ${result.status}\n`);
    console.log(`[${project.name}] ${name}: exit ${result.status} in ${seconds.toFixed(1)} s`);
    return { seconds, status: result.status, stdout: result.stdout ?? "" };
}

// StrykerJS and its runners, apart from the project.
const tools = join(work, "stryker-tools");
if (!existsSync(join(tools, "node_modules/.bin/stryker"))) {
    mkdirSync(tools, { recursive: true });
    writeFileSync(join(tools, "package.json"), '{"private":true}');
    // StrykerJS reads a project's tsconfig.json with TypeScript.
    run("tools-install", ["npm", "install", "--no-audit", "--no-fund", "typescript@5", ...["core", "mocha-runner", "jest-runner", "tap-runner"].map((name) => `@stryker-mutator/${name}@${corpus.stryker}`)], { cwd: tools });
}

const directory = join(work, project.name);
if (!existsSync(directory)) {
    mkdirSync(directory, { recursive: true });
    execFileSync("git", ["init", "-q"], { cwd: directory });
    execFileSync("git", ["fetch", "-q", "--depth", "1", project.repo, project.commit], { cwd: directory });
    execFileSync("git", ["checkout", "-q", "FETCH_HEAD"], { cwd: directory });
}
const install = run("install", project.install, { cwd: directory });
if (install.status !== 0)
    process.exit(1);
if (project.browsers)
    // A Playwright older than the runner's system knows no browser build for
    // it; the latest Ubuntu's it knows runs there.
    run("browsers", ["npx", "playwright", "install", "chromium", "chromium-headless-shell"], { cwd: directory, env: { PLAYWRIGHT_HOST_PLATFORM_OVERRIDE: "ubuntu24.04-x64" } });
const plain = run("plain-tests", project.test, { cwd: directory });

// StrykerJS: every killer of every mutant.
const include = project.mutate.filter((glob) => !glob.startsWith("!"));
const exclude = project.mutate.filter((glob) => glob.startsWith("!")).map((glob) => glob.slice(1));
const runnerPlugin = { mocha: "mocha-runner", jest: "jest-runner", tap: "tap-runner" }[project.stryker.testRunner];
writeFileSync(join(directory, "stryker.supercov-bench.json"), JSON.stringify({
    disableBail: true,
    incremental: false,
    reporters: ["json"],
    mutate: project.mutate,
    ignorePatterns: [".supercov"],
    tempDirName: ".stryker-tmp",
    // StrykerJS imports a plugin by what it is given: the module's own file.
    plugins: runnerPlugin ? [pathToFileURL(createRequire(join(tools, "package.json")).resolve(`@stryker-mutator/${runnerPlugin}`)).href] : [],
    ...project.stryker,
}, null, 2));
const stryker = args["skip-stryker"] ? null : run("stryker", [join(tools, "node_modules/.bin/stryker"), "run", "stryker.supercov-bench.json"], { cwd: directory });
const reportPath = join(directory, "reports/mutation/mutation.json");
if (existsSync(reportPath))
    cpSync(reportPath, join(out, "stryker-mutation.json"));

// Supercov: measure with the mutants' values, then the strong pass.
const environment = { SUPERCOV_SOURCE_ROOTS: project.sources };
const measure = run("supercov-measure", [supercov, "--", ...project.test], { cwd: directory, env: { ...environment, SUPERCOV_MUTANTS: "1" } });
const strong = run("supercov-strong", [supercov, "runs", "latest", "mutants", "test", "--json"], { cwd: directory, env: environment });
run("supercov-export", [supercov, "runs", "latest", "mutants", "--format", "jsonl", "--output", join(out, "supercov-mutants.jsonl")], { cwd: directory, env: environment });
let pass = {};
try {
    pass = JSON.parse(strong.stdout);
}
catch {
    // The log has it.
}
const strongDirectory = join(directory, ".supercov/strong");
const driverSummaries = existsSync(strongDirectory) ? readdirSync(strongDirectory).flatMap((name) => {
    try {
        return readFileSync(join(strongDirectory, name, "results.jsonl"), "utf8").split("\n").filter((line) => line.includes('"summary":true')).map((line) => JSON.parse(line));
    }
    catch {
        return [];
    }
}) : [];

// The join.
const summary = {
    project: project.name, runner: project.runner, commit: project.commit,
    cpus: availableParallelism(),
    seconds: { plainTests: plain.seconds, stryker: stryker?.seconds ?? null, supercovMeasure: measure.seconds, supercovStrong: strong.seconds },
    supercovSummary: pass.summary ?? null,
    driver: driverSummaries,
};
if (existsSync(reportPath) && existsSync(join(out, "supercov-mutants.jsonl"))) {
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    const strykerMutants = [];
    for (const [file, entry] of Object.entries(report.files)) {
        for (const mutant of entry.mutants) {
            const { start, end } = mutant.location;
            strykerMutants.push({
                file, span: `${start.line}:${start.column}-${end.line}:${end.column}`, mutator: mutant.mutatorName,
                replacement: (mutant.replacement ?? "").replace(/\s+/g, ""), rawReplacement: mutant.replacement ?? "",
                original: originalCode(entry.source, mutant.location), status: mutant.status, killedBy: mutant.killedBy ?? [],
            });
        }
    }
    const supercovMutants = readSupercovMutants(readFileSync(join(out, "supercov-mutants.jsonl"), "utf8"), { include, exclude });
    const { pairs, supercovOnly, strykerOnly, ambiguous } = joinMutants(supercovMutants, strykerMutants);
    const detected = (status) => status === "Killed" || status === "Timeout";
    const counts = {};
    const disagreements = [];
    for (const [ours, theirs] of pairs) {
        const key = `${ours.status}/${theirs.status}`;
        counts[key] = (counts[key] ?? 0) + 1;
        if (ours.status !== "Equivalent" && detected(ours.status) !== detected(theirs.status))
            disagreements.push({ id: ours.id, supercov: ours.status, stryker: theirs.status, killedBy: ours.killedBy, strykerKillers: theirs.killedBy.length, reason: ours.reason });
    }
    const strykerOnlyByMutator = {};
    for (const mutant of strykerOnly)
        strykerOnlyByMutator[mutant.mutator] = (strykerOnlyByMutator[mutant.mutator] ?? 0) + 1;
    Object.assign(summary, {
        mutants: { supercov: supercovMutants.length, stryker: strykerMutants.length, joined: pairs.length },
        supercovOnly: supercovOnly.map((mutant) => mutant.id), strykerOnlyByMutator, ambiguous, counts, disagreements,
        perMutantMs: {
            stryker: stryker ? Math.round(stryker.seconds * 1000 / Math.max(1, strykerMutants.length)) : null,
            supercovStrong: Math.round(strong.seconds * 1000 / Math.max(1, pass.summary?.ran ?? 1)),
        },
    });
}
writeFileSync(join(out, "summary.json"), JSON.stringify(summary, null, 2));
console.log(JSON.stringify({ project: summary.project, seconds: summary.seconds, counts: summary.counts, disagreements: summary.disagreements?.length }, null, 1));
