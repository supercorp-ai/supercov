import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { inferTestProvenance } from "./provenance.js";
import { atomicWriteFileSync } from "./atomic.js";
const GENERATED_EVIDENCE_DIRECTORY = "__SUPERCOV_EVIDENCE_DIRECTORY__";
/** Records outcomes even when browser or fixture startup fails before coverage. */
export default class SupercovPlaywrightReporter {
    onTestEnd(test, result) {
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
        const safeId = test.id.replace(/[^a-zA-Z0-9_-]/g, "_");
        const directory = resolve(process.cwd(), evidenceDirectory, `playwright-${safeId}-${result.retry}-status`);
        mkdirSync(directory, { recursive: true });
        atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
    }
}
