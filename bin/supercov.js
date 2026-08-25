#!/usr/bin/env node

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolveNativeBinary } from "./native.js";

const cli = fileURLToPath(new URL("../dist/cli.js", import.meta.url));
const runtime = fileURLToPath(new URL("../dist", import.meta.url));
const useRust = process.env["SUPERCOV_ENGINE"] === "rust";
let rustBinary;
if (useRust) {
  try {
    rustBinary = resolveNativeBinary();
  } catch (error) {
    console.error(`[supercov] ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}
const child = spawn(
  useRust ? rustBinary : process.execPath,
  [...(useRust ? [] : [cli]), ...process.argv.slice(2)],
  {
    stdio: "inherit",
    env: {
      ...process.env,
      ...(useRust && !process.env["SUPERCOV_RUNTIME_ROOT"]
        ? { SUPERCOV_RUNTIME_ROOT: runtime }
        : {}),
    },
  },
);

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    try {
      child.kill(signal);
    } catch {
      // The child may already have completed.
    }
  });
}

child.on("error", (error) => {
  console.error("[supercov] failed to start", error);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exitCode = code ?? 1;
});
