// First-party query transport, never executed by the test runner.
import { readFileSync } from "node:fs";
import { checkedIdentity } from "./identity.mjs";

try {
  const analyzer = checkedIdentity();
  const input = JSON.parse(readFileSync(0, "utf8"));
  // Check the build before importing executable analyzer code.
  const { compilerIdentity } = await import("./compiler-identity.mjs");
  const { analyzeArchive } = await import("../dist/archive.js");
  const compiler = compilerIdentity(input.projectRoot);
  const compilerSha256 = compiler.sha256;
  const result = analyzeArchive(input);
  if (
    JSON.stringify(compilerIdentity(input.projectRoot)) !==
      JSON.stringify(compiler) ||
    JSON.stringify(checkedIdentity()) !== JSON.stringify(analyzer)
  )
    throw new Error("Analyzer or compiler changed during analysis");
  process.stdout.write(
    JSON.stringify({
      ...result,
      analyzer: { ...analyzer, compilerSha256, compiler },
    }),
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
