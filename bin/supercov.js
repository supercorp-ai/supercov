#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const cli = fileURLToPath(new URL("../dist/cli.js", import.meta.url));
const runtime = fileURLToPath(new URL("../dist", import.meta.url));
const rustBinary =
  process.env["SUPERCOV_RUST_BINARY"] ??
  fileURLToPath(
    new URL(
      `../target/debug/supercov${process.platform === "win32" ? ".exe" : ""}`,
      import.meta.url,
    ),
  );
const useRust = process.env["SUPERCOV_ENGINE"] === "rust";
if (useRust && !existsSync(rustBinary)) {
  console.error(
    `[supercov] Rust engine candidate binary not found at ${rustBinary}. Build it with cargo build -p supercov-cli or set SUPERCOV_RUST_BINARY.`,
  );
  process.exit(1);
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
