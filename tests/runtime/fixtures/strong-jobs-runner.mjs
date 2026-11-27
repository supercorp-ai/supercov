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
//   builds Rust compiled to wasm holds Cargo's on its build directory),
//   "loaded" (once any job with a mutant has started, every job's test
//   takes 2.5 s: a machine other work made slower after the baselines ran),
//   "stops" (marks the pass as told to stop, `stopped-at` beside its
//   results, and the process ends, nothing written: a job the stop ended).
// A test id may also carry a second rule after a comma: "slow,subset-fail".
import { appendFileSync, existsSync, linkSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
        if (also === "loaded" || rule === "loaded") {
            const slowed = join(dirname(process.env.FAKE_SUITE), "slowed");
            if (mutant !== "-")
                writeFileSync(slowed, "");
            if (existsSync(slowed))
                await new Promise((done) => setTimeout(done, 2_500));
        }
        if (also === "locked" || rule === "locked") {
            // Released when its holder ends, however it ends, as Cargo's is.
            // The lock is taken by linking a file that already holds this
            // process's pid, so it never exists without its holder's pid: a
            // lock created and then written, its holder stopped in between,
            // was an empty file whose holder read as pid 0 -- this process's
            // own group, always alive -- and every job after it waited on it
            // until its limit.
            const lock = join(dirname(process.env.FAKE_SUITE), "build.lock");
            const mine = `${lock}.${process.pid}`;
            writeFileSync(mine, String(process.pid));
            for (;;) {
                try {
                    linkSync(mine, lock);
                    break;
                }
                catch {
                    try {
                        process.kill(Number(readFileSync(lock, "utf8")), 0);
                    }
                    catch (error) {
                        // Gone, or held by a process that ended.
                        if (error.code !== "EPERM")
                            rmSync(lock, { force: true });
                    }
                    await new Promise((done) => setTimeout(done, 10));
                }
            }
            rmSync(mine);
            await new Promise((done) => setTimeout(done, 800));
            rmSync(lock);
        }
        if (rule === "hang")
            await new Promise(() => setInterval(() => { }, 1000));
        if (rule === "crash")
            process.exit(3);
        if (rule === "stops") {
            writeFileSync(join(dirname(process.env.FAKE_SUITE), "stopped-at"), String(Date.now()));
            process.exit(3);
        }
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
