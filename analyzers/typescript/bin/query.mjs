// First-party query transport, never executed by the test runner.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { checkedIdentity } from "./identity.mjs";

try {
  const analyzer = checkedIdentity();
  const input = JSON.parse(readFileSync(0, "utf8"));
  // Check the build before importing executable analyzer code.
  const { analyzeArchive } = await import("../dist/archive.js");
  const { projectCompilerPath } = await import("../dist/compiler.js");
  const compilerFile = projectCompilerPath(input.projectRoot);
  const hashCompiler = () =>
    createHash("sha256").update(readFileSync(compilerFile)).digest("hex");
  const compilerSha256 = hashCompiler();
  const result = analyzeArchive(input);
  if (
    hashCompiler() !== compilerSha256 ||
    JSON.stringify(checkedIdentity()) !== JSON.stringify(analyzer)
  )
    throw new Error("Analyzer or compiler changed during analysis");
  process.stdout.write(
    JSON.stringify({ ...result, analyzer: { ...analyzer, compilerSha256 } }),
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
