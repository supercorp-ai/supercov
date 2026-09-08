import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { inferTestProvenance } from "./provenance.mjs";
import { atomicWriteFileSync } from "./atomic.mjs";
function digest(value) {
    return createHash("sha256").update(value).digest("hex").slice(0, 24);
}
function localFile(path) {
    return relative(process.cwd(), path).split(sep).join("/");
}
function attemptStatus(status) {
    switch (status) {
        case "passed":
        case "failed":
            return status;
        case "pending":
        case "skipped":
        case "todo":
        case "disabled":
            return "skipped";
        default:
            return "unknown";
    }
}
/**
 * Records each Jest attempt's outcome, which the test environment never
 * learns: a test that times out, one skipped before its hooks, one that
 * failed after its last hook. The test id formula must match jest.cjs, whose
 * record carries the attempt's coverage; the report joins the two.
 */
export default class SupercovJestReporter {
    onTestCaseResult(test, result) {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        if (!evidenceDirectory)
            return;
        const testFile = localFile(test.path);
        const testId = `jest:${digest(`${testFile}\0${result.fullName}`)}`;
        const retry = Math.max((result.invocations ?? 1) - 1, 0);
        const status = attemptStatus(result.status);
        const payload = {
            testId,
            test: result.fullName,
            testFile,
            title: result.title,
            retry,
            status,
            expectedStatus: "passed",
            flaky: retry > 0 && status === "passed",
            provenance: inferTestProvenance({
                runner: "jest",
                file: testFile,
                explicitKind: process.env["SUPERCOV_TEST_KIND"],
            }),
            runtime: [],
            browser: [],
            server: [],
        };
        const directory = resolve(process.cwd(), evidenceDirectory, `jest-${digest(testId)}-${retry}-status`);
        mkdirSync(directory, { recursive: true });
        atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
    }
    // Jest calls these on every reporter.
    onRunComplete() { }
    getLastError() {
        return undefined;
    }
}
