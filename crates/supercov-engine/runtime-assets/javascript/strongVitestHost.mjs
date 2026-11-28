// A resident Vitest for a strong pass's Browser Mode jobs (strongJobs.mjs):
// one process that keeps Vitest, its Vite server and its browser running and
// runs one job after another, where each job used to start the command, and
// with it Vitest, Vite and Chromium, again.
//
// What a fresh command gave each job, this keeps:
//
// - A fresh page and a fresh browser context for every job. Vitest reuses
//   its browser sessions from one run to the next, and with them the
//   context's cookies, storage, permissions and routes; before each job the
//   last job's pages and contexts are closed and Vitest's sessions for them
//   forgotten, so the run opens new ones. After the run, each context that
//   served it must be one no earlier job used, or the job's outcome is
//   thrown away and it runs as a command of its own.
// - The mutant switched on in the page before the test file loads, through
//   Supercov's browser command, which reads the job from the environment
//   this process sets for each job.
// - Nothing of the project's code loaded in this process before a job: the
//   job's mutant is on in every process of a command from its start, and a
//   module this process loaded earlier ran its top level without it. A job
//   starts here only while no instrumented module has loaded in node.
//
// It refuses (and the driver starts the command for the job, as before)
// where it cannot keep that: a Vitest without the runner API it drives
// (before 4), a browser provider other than Playwright's or a persistent
// context, a global setup, browser commands of the project's own, a Vite
// plugin of the project's that serves requests itself (a mock API keeps what
// one job left for the next), or a job file that a project without a
// browser runs too.
//
// The driver forks it in the project's copy and sends
//   { type: "init", args: [<the command's arguments to vitest>] }
//   { type: "job", id, file: <absolute test file>, mutant: { file, index } | null, environment: {...} }
// and it answers { type: "ready", error? } and { type: "done", id, error?, fallback?, tainted? }:
// `fallback` that the job did not run here (or ran where its outcome cannot
// stand) and runs as a command of its own; `tainted` that this process can
// run no further job.
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const root = process.cwd();

// Vite plugins known to keep no state a request can change: Vite's own,
// Vitest's, and the frameworks' compilers.
const STATELESS_PLUGIN = /^(?:vite:|vitest|@vitest\/|@vitejs\/|vite-plugin-svelte|vite-plugin-solid|solid|svelte|vue|react|preact|supercov)/;

class Refusal extends Error {
}

let vitest;
// Every browser context a job ran in.
const used = new WeakSet();

// The modules of the project's code this process loaded: each instrumented
// module takes its slot as it loads.
function loadedSources() {
    return globalThis[Symbol.for("supercov.strongMutation")]?.slots?.size ?? 0;
}

function browserProjects() {
    return vitest.projects.filter((project) => project.config?.browser?.enabled);
}

// The browser provider's sessions: what Vitest keeps for each, where this
// Vitest keeps it as this process knows how to reset.
function sessionsOf(project) {
    const provider = project.browser?.provider;
    const orchestrators = project.browser?.state?.orchestrators;
    const ready = project._browserReadySessions;
    const ids = vitest._browserSessions?.sessionIds;
    if (provider?.name !== "playwright" || !(provider.pages instanceof Map) || !(provider.contexts instanceof Map) || provider.persistentContext)
        throw new Refusal("its browser provider is not Playwright's with a context per session");
    if (!(orchestrators instanceof Map) || !(ready instanceof Set) || !(ids instanceof Set))
        throw new Refusal("this Vitest keeps its browser sessions where the pass cannot reset them");
    return { provider, orchestrators, ready, ids };
}

async function init(args) {
    process.env.SUPERCOV_INSIDE_VITEST = "1";
    process.env.SUPERCOV_STRONG_PLAN = "";
    process.env.SUPERCOV_ACTIVE_MUTANT = "-";
    const generated = process.env.SUPERCOV_GENERATED_VITEST_CONFIG;
    if (!generated || !existsSync(generated))
        throw new Refusal("no generated Vitest configuration");
    const fromRoot = createRequire(join(root, "package.json"));
    const node = await import(pathToFileURL(fromRoot.resolve("vitest/node")).href);
    if (typeof node.parseCLI !== "function" || typeof node.createVitest !== "function")
        throw new Refusal("this Vitest has no runner API");
    const { options } = node.parseCLI(["vitest", ...args]);
    // The project's own configuration, read by Supercov's, as runnerEntry.mjs
    // hands it to a command.
    if (options.config && resolve(root, String(options.config)) !== resolve(generated))
        process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG = resolve(root, String(options.config));
    delete options.config;
    vitest = await node.createVitest("test", { ...options, config: generated, watch: false, run: true });
    if (typeof vitest.runTestSpecifications !== "function" || typeof vitest.globTestSpecifications !== "function")
        throw new Refusal("this Vitest cannot run a file it is given (before 3)");
    // Reporters' onInit (Supercov's sets each project's setup file).
    await (typeof vitest.standalone === "function" ? vitest.standalone() : vitest.init());
    if (vitest.config.globalSetup?.length || vitest.projects.some((project) => project.config.globalSetup?.length))
        throw new Refusal("a global setup runs once for every job here");
    for (const project of browserProjects()) {
        const commands = Object.keys(project.config.browser?.commands ?? {}).filter((name) => !name.startsWith("__supercov"));
        if (commands.length)
            throw new Refusal(`the project's own browser commands (${commands.join(", ")}) may keep state between jobs`);
        const serving = (project.vite?.config?.plugins ?? []).filter((plugin) => plugin?.configureServer && !STATELESS_PLUGIN.test(String(plugin.name ?? "")));
        if (serving.length)
            throw new Refusal(`a Vite plugin serves requests itself (${serving.map((plugin) => plugin.name).join(", ")})`);
    }
    if (!browserProjects().length)
        throw new Refusal("no project runs in a browser");
}

// Closes the last job's pages and contexts, and forgets Vitest's sessions
// for them: the next run opens new ones.
async function freshSessions() {
    for (const project of browserProjects()) {
        if (!project.browser)
            continue;
        const { provider, orchestrators, ready, ids } = sessionsOf(project);
        const sessions = new Set([...provider.pages.keys(), ...provider.contexts.keys(), ...orchestrators.keys(), ...ready]);
        for (const sessionId of sessions) {
            const page = provider.pages.get(sessionId);
            const context = provider.contexts.get(sessionId);
            provider.pages.delete(sessionId);
            provider.contexts.delete(sessionId);
            await page?.close().catch(() => { });
            await context?.close().catch(() => { });
            ready.delete(sessionId);
            ids.delete(sessionId);
        }
        // The orchestrator's socket closes with its page, and Vitest forgets it.
        const until = Date.now() + 10_000;
        while (orchestrators.size > 0) {
            if (Date.now() > until)
                throw new Refusal("a browser session did not close");
            await new Promise((done) => setTimeout(done, 10));
        }
    }
}

// Each context that served the run, never one an earlier job used.
function contextsWereFresh() {
    let fresh = true;
    for (const project of browserProjects()) {
        if (!project.browser)
            continue;
        const { provider, orchestrators } = sessionsOf(project);
        for (const sessionId of orchestrators.keys()) {
            const context = provider.contexts.get(sessionId);
            if (!context || used.has(context))
                fresh = false;
            else
                used.add(context);
        }
    }
    return fresh;
}

async function runJob({ file, mutant, environment }) {
    // A module of the project's code loaded here ran its top level without
    // this job's mutant.
    if (loadedSources() > 0)
        return { fallback: "the project's code loaded in Vitest's process", tainted: true };
    // The file as the command names it, from the project's root.
    const specifications = await vitest.globTestSpecifications([relative(root, file).split(sep).join("/")]);
    if (!specifications.length)
        return { fallback: "Vitest found no such test file" };
    if (specifications.some((specification) => !specification.project.config?.browser?.enabled))
        return { fallback: "a project without a browser runs the file too" };
    await freshSessions();
    Object.assign(process.env, environment, { SUPERCOV_ACTIVE_MUTANT: mutant ? `${mutant.file}#${mutant.index}` : "-" });
    if (mutant)
        runtime.setActiveMutant(mutant.file, mutant.index);
    else
        runtime.clearActiveMutant();
    try {
        await vitest.runTestSpecifications(specifications, false);
    }
    finally {
        runtime.clearActiveMutant();
    }
    if (!contextsWereFresh())
        return { fallback: "a browser context served two jobs", tainted: true };
    if (loadedSources() > 0)
        // This job's own loads ran with its mutant on, as a command's would;
        // the next job's would not.
        return { tainted: true };
    return {};
}

let queue = Promise.resolve();
process.on("message", (message) => {
    const reply = (answer) => process.send?.(answer);
    if (message?.type === "init") {
        init(message.args ?? []).then(() => reply({ type: "ready" }), (error) => {
            // A Vitest this process cannot drive is the command's to run,
            // job by job, as before.
            reply({ type: "ready", error: String(error instanceof Refusal ? error.message : error?.stack ?? error) });
        });
        return;
    }
    if (message?.type === "job") {
        queue = queue.then(() => runJob(message)).then((outcome) => reply({ type: "done", id: message.id, ...outcome }), (error) => reply({ type: "done", id: message.id, fallback: String(error?.stack ?? error), tainted: true }));
    }
});
process.send?.({ type: "started" });
