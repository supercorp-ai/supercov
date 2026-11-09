#!/usr/bin/env node

import { spawn } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveNativeBinary } from "./native.js";

let rustBinary;
try {
  rustBinary = resolveNativeBinary();
} catch (error) {
  console.error(`[supercov] ${error instanceof Error ? error.message : String(error)}`);
  process.exit(1);
}
const child = spawn(
  rustBinary,
  [...(separatorMayBeDropped() ? ["__separator-may-be-dropped"] : []), ...process.argv.slice(2)],
  {
    stdio: "inherit",
    env: {
      ...process.env,
      SUPERCOV_PACKAGE_ROOT: resolvePackageRoot(),
    },
  },
);

// Bun's runtime drops a leading `--` from the arguments it gives a script
// (oven-sh/bun#13984), and Yarn 1 drops the one in `yarn supercov -- ...`:
// `supercov -- bun test` reached the binary as `supercov bun test`. Neither
// can be seen from here, so the binary is told it may have happened, and
// takes a first word that is no command of its own but a program for the
// test command.
function separatorMayBeDropped() {
  return (
    typeof process.versions.bun === "string" ||
    /^yarn\/1\./.test(process.env.npm_config_user_agent ?? "")
  );
}

function resolvePackageRoot() {
  return dirname(dirname(fileURLToPath(import.meta.url)));
}

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
