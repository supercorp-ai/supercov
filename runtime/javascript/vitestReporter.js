import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { inferTestProvenance } from "./provenance.js";
import { atomicWriteFileSync } from "./atomic.js";
function sourcePath(moduleId) {
    const absolute = moduleId.startsWith("file:")
        ? fileURLToPath(moduleId)
        : moduleId;
    return relative(process.cwd(), absolute).split(sep).join("/");
}
/** Records final runner outcomes, including tests that never execute hooks. */
export default class SupercovVitestReporter {
    onTestCaseResult(testCase) {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        if (!evidenceDirectory)
            return;
        const result = testCase.result();
        if (result.state === "pending")
            return;
        const diagnostic = testCase.diagnostic();
        const testFile = sourcePath(testCase.module.moduleId);
        const retry = diagnostic?.retryCount ?? 0;
        const payload = {
            testId: `vitest:${testCase.id}`,
            test: testCase.fullName,
            testFile,
            title: testCase.name,
            retry,
            status: result.state,
            expectedStatus: testCase.options.fails ? "failed" : "passed",
            flaky: diagnostic?.flaky ?? false,
            provenance: inferTestProvenance({
                runner: "vitest",
                file: testFile,
                project: testCase.project.name,
                explicitKind: process.env["SUPERCOV_TEST_KIND"],
            }),
            runtime: [],
            browser: [],
            server: [],
        };
        const safeId = testCase.id.replace(/[^a-zA-Z0-9_-]/g, "_");
        const directory = resolve(process.cwd(), evidenceDirectory, `vitest-${safeId}-${retry}-status`);
        mkdirSync(directory, { recursive: true });
        atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
    }
    /** Vitest 2 compatibility; Vitest 3+ uses onTestCaseResult above. */
    onFinished(files = []) {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        if (!evidenceDirectory)
            return;
        const visit = (task, inheritedFile) => {
            const file = task.file ?? inheritedFile ?? task;
            if (task.type === "test" && task.result?.state) {
                const testFile = sourcePath(file.filepath ?? task.filepath ?? "unknown");
                const retry = task.result.retryCount ?? 0;
                const names = [task.name ?? task.id ?? "test"];
                let suite = task.suite;
                while (suite?.name) {
                    names.unshift(suite.name);
                    suite = suite.suite;
                }
                const payload = {
                    testId: `vitest:${task.id ?? names.join(" > ")}`,
                    test: names.join(" > "),
                    testFile,
                    title: task.name ?? names.at(-1) ?? "test",
                    retry,
                    status: task.result.state === "pass"
                        ? "passed"
                        : task.result.state === "fail"
                            ? "failed"
                            : "skipped",
                    provenance: inferTestProvenance({
                        runner: "vitest",
                        file: testFile,
                        project: file.projectName,
                        explicitKind: process.env["SUPERCOV_TEST_KIND"],
                    }),
                    runtime: [],
                    browser: [],
                    server: [],
                };
                const safeId = String(task.id ?? names.join("-")).replace(/[^a-zA-Z0-9_-]/g, "_");
                const directory = resolve(process.cwd(), evidenceDirectory, `vitest-${safeId}-${retry}-status`);
                mkdirSync(directory, { recursive: true });
                atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
            }
            for (const child of task.tasks ?? [])
                visit(child, file);
        };
        for (const file of files)
            visit(file, file);
    }
}
