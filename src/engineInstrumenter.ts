import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { instrumentMcdc, type InstrumentMcdcResult } from "./instrumenter.ts";
import {
  requireRustEngineBinary,
  runRustEngineJson,
  rustEngineEnabled,
} from "./engineProcess.ts";
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

export function instrumentationEngineIdentity(): {
  engine: "typescript" | "rust";
  artifact?: string;
} {
  if (!rustEngineEnabled()) return { engine: "typescript" };
  const binary = requireRustEngineBinary();
  return {
    engine: "rust",
    artifact: createHash("sha256").update(readFileSync(binary)).digest("hex"),
  };
}

function rustBatch(cases: InstrumentSource[]): InstrumentMcdcResult[] {
  if (cases.length === 0) return [];
  const outputs = runRustEngineJson<InstrumentSource[], RustCandidateOutput[]>(
    "__instrument-js",
    cases,
  );
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
  return rustEngineEnabled()
    ? rustBatch(cases)
    : cases.map(({ file, source }) => instrumentMcdc(source, file));
}

export function instrumentSource(
  source: string,
  file: string,
): InstrumentMcdcResult {
  return instrumentSources([{ file, source }])[0]!;
}
