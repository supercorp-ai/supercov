// A test runner for strong-jobs.test.mjs: it runs the tests a strong pass's
// job chooses, as an adapter would, and writes their outcomes where a job's
// adapter writes its evidence. What each test does is in the suite file
// beside the job (FAKE_SUITE): `files` lists each test file's tests in order,
// `rules` what a test does under a mutant (`-` for none), keyed by the
// mutant, then the test id:
//   "fail", "hang" (never ends), "loop" (passes its hit limit and says so),
//   "crash" (the process ends, nothing written), "subset-fail" (fails when
//   run without the tests before it in its file), "slow" (passes in 0.8 s),
//   "locked" (holds a lock every job shares for 0.8 s, as each job that
//   builds Rust compiled to wasm holds Cargo's on its build directory).
// A test id may also carry a second rule after a comma: "slow,subset-fail".
import { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const suite = JSON.parse(readFileSync(process.env.FAKE_SUITE, "utf8"));
const job = JSON.parse(readFileSync(process.env.SUPERCOV_STRONG_JOB, "utf8"));
const mutant = process.env.SUPERCOV_ACTIVE_MUTANT ?? "-";
const rules = { ...(suite.rules["*"] ?? {}), ...(suite.rules[mutant] ?? {}) };
const evidence = process.env.SUPERCOV_EVIDENCE_DIR;
// Each process a job starts, noted where FAKE_PIDS says.
if (process.env.FAKE_PIDS)
    appendFileSync(process.env.FAKE_PIDS, `${process.pid}\n`);
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
        const [rule, also] = String(rules[testId] ?? "").split(",").reverse();
        if (also === "slow" || rule === "slow")
            await new Promise((done) => setTimeout(done, 800));
        if (also === "locked" || rule === "locked") {
            // Released when its holder ends, however it ends, as Cargo's is.
            const lock = join(dirname(process.env.FAKE_SUITE), "build.lock");
            for (;;) {
                try {
                    writeFileSync(lock, String(process.pid), { flag: "wx" });
                    break;
                }
                catch {
                    try {
                        process.kill(Number(readFileSync(lock, "utf8")), 0);
                    }
                    catch {
                        rmSync(lock, { force: true });
                    }
                    await new Promise((done) => setTimeout(done, 10));
                }
            }
            await new Promise((done) => setTimeout(done, 800));
            rmSync(lock);
        }
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
