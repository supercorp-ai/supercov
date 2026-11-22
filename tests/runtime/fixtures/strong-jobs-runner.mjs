// A test runner for strong-jobs.test.mjs: it runs the tests a strong pass's
// job chooses, as an adapter would, and writes their outcomes where a job's
// adapter writes its evidence. What each test does is in the suite file
// beside the job (FAKE_SUITE): `files` lists each test file's tests in order,
// `rules` what a test does under a mutant (`-` for none), keyed by the
// mutant, then the test id:
//   "fail", "hang" (never ends), "loop" (passes its hit limit and says so),
//   "crash" (the process ends, nothing written), "subset-fail" (fails when
//   run without the tests before it in its file).
import { appendFileSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const suite = JSON.parse(readFileSync(process.env.FAKE_SUITE, "utf8"));
const job = JSON.parse(readFileSync(process.env.SUPERCOV_STRONG_JOB, "utf8"));
const mutant = process.env.SUPERCOV_ACTIVE_MUTANT ?? "-";
const rules = { ...(suite.rules["*"] ?? {}), ...(suite.rules[mutant] ?? {}) };
const evidence = process.env.SUPERCOV_EVIDENCE_DIR;
mkdirSync(evidence, { recursive: true });
const write = (testId, status) => appendFileSync(join(evidence, "fake.mcdc.jsonl"), `${JSON.stringify({ testId, status, retry: 0 })}\n`);
let failed = false;
for (const [file, chosen] of Object.entries(job.files)) {
    const all = suite.files[file] ?? [];
    const running = chosen === null ? all : all.filter((id) => chosen.includes(id));
    for (const [at, testId] of running.entries()) {
        if (job.bail && failed) {
            write(testId, "skipped");
            continue;
        }
        const rule = rules[testId];
        if (rule === "hang")
            await new Promise(() => setInterval(() => { }, 1000));
        if (rule === "crash")
            process.exit(3);
        if (rule === "loop") {
            appendFileSync(`${process.env.SUPERCOV_STRONG_RESULTS}.timeouts.jsonl`, `${JSON.stringify({ mutant, attemptId: null, pass: "main" })}\n`);
            write(testId, "passed");
            continue;
        }
        // Fails where the test before it in the file did not run first.
        const alone = rule === "subset-fail" && all.indexOf(testId) > 0 && running[at - 1] !== all[all.indexOf(testId) - 1];
        const fails = rule === "fail" || alone;
        if (fails)
            failed = true;
        write(testId, fails ? "failed" : "passed");
    }
}
