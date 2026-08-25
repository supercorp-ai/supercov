import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export function rustEngineEnabled(): boolean {
  return process.env["SUPERCOV_ENGINE"] === "rust";
}

export function rustEngineBinaryPath(): string {
  const configured = process.env["SUPERCOV_RUST_BINARY"];
  if (configured) return configured;
  return fileURLToPath(new URL("../target/debug/supercov", import.meta.url));
}

export function requireRustEngineBinary(): string {
  const binary = rustEngineBinaryPath();
  if (!existsSync(binary))
    throw new Error(
      `Rust engine candidate binary not found at ${binary}. Build it with cargo build -p supercov or set SUPERCOV_RUST_BINARY.`,
    );
  return binary;
}

/**
 * Temporary private migration transport. Product behavior remains
 * fire-and-forget; this child boundary disappears when the Rust CLI owns the
 * whole command. Keeping one generic, bounded JSON exchange avoids creating a
 * bespoke Node implementation for every Rust-owned engine slice meanwhile.
 */
export function runRustEngineJson<Input, Output>(
  command: string,
  input: Input,
  maxBuffer = 1024 * 1024 * 1024,
): Output {
  const child = spawnSync(requireRustEngineBinary(), [command], {
    input: JSON.stringify(input),
    encoding: "utf8",
    maxBuffer,
    env: { ...process.env, SUPERCOV_INTERNAL_ENGINE: "1" },
  });
  if (child.error) throw child.error;
  if (child.status !== 0)
    throw new Error(
      `Rust engine command ${command} failed with exit ${child.status}: ${child.stderr.trim()}`,
    );
  try {
    return JSON.parse(child.stdout) as Output;
  } catch (error) {
    throw new Error(`Rust engine command ${command} returned invalid JSON`, {
      cause: error,
    });
  }
}
