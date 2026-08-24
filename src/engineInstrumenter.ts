import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { instrumentMcdc, type InstrumentMcdcResult } from "./instrumenter.ts";
import type { CoverageManifest } from "./types.ts";

export interface InstrumentSource {
  file: string;
  source: string;
}

interface RustCandidateOutput {
  engine: string;
  complete: boolean;
  code: string;
  map?: unknown;
  decisions: CoverageManifest["decisions"];
  points: CoverageManifest["points"];
  branches: CoverageManifest["branches"];
  coverageLimitations: NonNullable<CoverageManifest["limitations"]>;
}

export function rustInstrumenterEnabled(): boolean {
  return process.env["SUPERCOV_ENGINE"] === "rust";
}

export function rustInstrumenterBinaryPath(): string {
  const configured = process.env["SUPERCOV_RUST_BINARY"];
  if (configured) return configured;
  return fileURLToPath(new URL("../target/debug/supercov", import.meta.url));
}

export function instrumentationEngineIdentity(): {
  engine: "typescript" | "rust";
  artifact?: string;
} {
  if (!rustInstrumenterEnabled()) return { engine: "typescript" };
  const binary = rustInstrumenterBinaryPath();
  if (!existsSync(binary))
    throw new Error(
      `Rust engine candidate binary not found at ${binary}. Build it with cargo build -p supercov-cli or set SUPERCOV_RUST_BINARY.`,
    );
  return {
    engine: "rust",
    artifact: createHash("sha256").update(readFileSync(binary)).digest("hex"),
  };
}

function rustBatch(cases: InstrumentSource[]): InstrumentMcdcResult[] {
  if (cases.length === 0) return [];
  const binary = rustInstrumenterBinaryPath();
  if (!existsSync(binary))
    throw new Error(
      `Rust engine candidate binary not found at ${binary}. Build it with cargo build -p supercov-cli or set SUPERCOV_RUST_BINARY.`,
    );
  const child = spawnSync(binary, ["__instrument-js"], {
    input: JSON.stringify(cases),
    encoding: "utf8",
    maxBuffer: 1024 * 1024 * 1024,
    env: { ...process.env, SUPERCOV_INTERNAL_INSTRUMENTER: "1" },
  });
  if (child.error) throw child.error;
  if (child.status !== 0)
    throw new Error(
      `Rust engine candidate failed with exit ${child.status}: ${child.stderr.trim()}`,
    );
  let outputs: RustCandidateOutput[];
  try {
    outputs = JSON.parse(child.stdout) as RustCandidateOutput[];
  } catch (error) {
    throw new Error("Rust engine candidate returned invalid JSON", { cause: error });
  }
  if (outputs.length !== cases.length)
    throw new Error(
      `Rust engine candidate returned ${outputs.length} result(s) for ${cases.length} input(s)`,
    );
  return outputs.map((output, index) => {
    if (output.engine !== "rust-oxc")
      throw new Error(`Unexpected Rust engine identity for ${cases[index]!.file}`);
    return {
      code: output.code,
      map: output.map as InstrumentMcdcResult["map"],
      decisions: output.decisions,
      manifest: {
        decisions: output.decisions,
        points: output.points,
        branches: output.branches,
        ...(output.coverageLimitations.length > 0
          ? { limitations: output.coverageLimitations }
          : {}),
      },
    };
  });
}

/**
 * Frozen migration boundary shared by direct and build-tool instrumentation.
 * Rust receives one whole batch whenever the caller already has an inventory.
 */
export function instrumentSources(
  cases: InstrumentSource[],
): InstrumentMcdcResult[] {
  return rustInstrumenterEnabled()
    ? rustBatch(cases)
    : cases.map(({ file, source }) => instrumentMcdc(source, file));
}

export function instrumentSource(
  source: string,
  file: string,
): InstrumentMcdcResult {
  return instrumentSources([{ file, source }])[0]!;
}
