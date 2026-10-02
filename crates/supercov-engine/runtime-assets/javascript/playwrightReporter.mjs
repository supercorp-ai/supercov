import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { inferTestProvenance } from "./provenance.mjs";
import { appendJsonLineDurableSync, appendJsonLineSync } from "./atomic.mjs";
const GENERATED_EVIDENCE_DIRECTORY = "__SUPERCOV_EVIDENCE_DIRECTORY__";
const evidenceWriterIdentity = () => (process.env.SUPERCOV_EXECUTION_LOG_SHARD ?? `pid-${process.pid}`)
    .replace(/[^A-Za-z0-9_-]/g, "_");
// Playwright's own code frame builder, from the copy that runs the tests.
let frameBuilder;
function codeFrame() {
    if (frameBuilder !== undefined)
        return frameBuilder;
    try {
        const require = createRequire(join(process.cwd(), "noop.js"));
        const root = dirname(require.resolve("playwright/package.json"));
        frameBuilder = require(join(root, "lib/transform/babelBundle.js")).codeFrameColumns ?? null;
    }
    catch {
        frameBuilder = null;
    }
    return frameBuilder;
}
/**
 * Playwright prints the failing line from the file it ran: Supercov's copy in
 * the workspace, where assertions carry the wrappers that time them. Show the
 * project's own line instead. Every line keeps its number in the copy.
 */
export function originalSnippet(error, workspace = process.env.SUPERCOV_PROJECT_ROOT, source = process.env.SUPERCOV_SOURCE_PROJECT_ROOT, build = codeFrame()) {
    const location = error?.location;
    if (!error?.snippet || !location?.file || !workspace || !source || !build)
        return;
    const path = relative(workspace, location.file);
    if (!path || path.startsWith("..") || isAbsolute(path))
        return;
    let copied;
    let original;
    try {
        copied = readFileSync(location.file, "utf8");
        original = readFileSync(join(source, path), "utf8");
    }
    catch {
        return;
    }
    if (copied === original)
        return;
    const originalLine = original.split("\n")[location.line - 1];
    if (originalLine === undefined)
        return;
    // Playwright reads the copy's inline source map, so the location is the
    // original line. The map has one point per copied run of text, so the
    // column is the start of the statement; Playwright points at the matcher,
    // which the message names (`expect(received).toBe(expected)`).
    let column = location.column <= originalLine.length + 1
        ? location.column
        : originalLine.length - originalLine.trimStart().length + 1;
    const matcher = /expect\(.*?\)\.(?:not\.|resolves\.|rejects\.|soft\.|poll\.)*(\w+)\(/.exec(String(error.message ?? "").replace(/\u001b\[[0-9;]*m/g, ""))?.[1];
    const named = matcher ? originalLine.indexOf(`.${matcher}(`, column - 1) : -1;
    if (named >= 0)
        column = named + 2;
    const start = { line: location.line, column };
    const shown = build(copied, { start: location }, { highlightCode: true });
    if (!error.snippet.includes(shown))
        return;
    error.snippet = error.snippet.replace(shown, build(original, { start }, { highlightCode: true }));
}
/** Records outcomes even when browser or fixture startup fails before coverage. */
export default class SupercovPlaywrightReporter {
    records = [];
    onTestEnd(test, result) {
        // Reporters print failures at the end of the run, after every
        // reporter has seen this test.
        for (const error of result.errors ?? [])
            originalSnippet(error);
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"] ??
            (GENERATED_EVIDENCE_DIRECTORY.startsWith("__")
                ? undefined
                : GENERATED_EVIDENCE_DIRECTORY);
        if (!evidenceDirectory)
            return;
        const testFile = relative(process.cwd(), test.location.file)
            .split(sep)
            .join("/");
        const payload = {
            testId: test.id,
            test: test.titlePath().filter(Boolean).join(" > "),
            testFile,
            title: test.title,
            retry: result.retry,
            status: result.status ?? "unknown",
            expectedStatus: test.expectedStatus,
            provenance: inferTestProvenance({
                runner: "playwright",
                file: testFile,
                project: test.parent.project()?.name,
                explicitKind: process.env["SUPERCOV_TEST_KIND"],
            }),
            browser: [],
            server: [],
        };
        this.records.push(payload);
    }
    onEnd() {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"] ??
            (GENERATED_EVIDENCE_DIRECTORY.startsWith("__")
                ? undefined
                : GENERATED_EVIDENCE_DIRECTORY);
        if (!evidenceDirectory || this.records.length === 0)
            return;
        const append = process.env.SUPERCOV_DURABLE_EVIDENCE_EACH_TEST === "1"
            ? appendJsonLineDurableSync
            : appendJsonLineSync;
        append(resolve(process.cwd(), evidenceDirectory, `playwright-status-${evidenceWriterIdentity()}-${process.pid}.mcdc.jsonl`), `${this.records.map(record => JSON.stringify(record)).join("\n")}\n`);
    }
}
