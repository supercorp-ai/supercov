#!/usr/bin/env node

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const cli = fileURLToPath(new URL("../dist/cli.js", import.meta.url));
const child = spawn(
  process.execPath,
  [cli, ...process.argv.slice(2)],
  {
    stdio: "inherit",
    env: process.env,
  },
);

child.on("error", (error) => {
  console.error("[supercov] failed to start", error);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exitCode = code ?? 1;
});
