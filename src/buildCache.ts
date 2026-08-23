import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { isAbsolute, resolve, sep } from "node:path";
import { atomicWriteFileSync } from "./atomic.ts";
import type { CoverageRunIntegrity } from "./types.ts";
import type { CoverageProject } from "./project.ts";

export const INSTRUMENTED_BUILD_CACHE_SCHEMA_VERSION = 1;

const BUILD_OUTPUT_CANDIDATES = [
  "build",
  "dist",
  ".next",
  ".nuxt",
  ".output",
];

export interface InstrumentedBuildCache {
  schemaVersion: typeof INSTRUMENTED_BUILD_CACHE_SCHEMA_VERSION;
  key: string;
  createdAt: string;
  artifactPaths: string[];
}

function metadataPath(workspace: string): string {
  return resolve(workspace, ".supercov/build-cache.json");
}

function safeRelativePath(path: string): boolean {
  return Boolean(
    path &&
      !isAbsolute(path) &&
      path !== ".." &&
      !path.startsWith(`..${sep}`) &&
      !path.split(/[\\/]/).includes(".."),
  );
}

export function instrumentedBuildCacheKey(
  integrity: CoverageRunIntegrity,
  project: CoverageProject,
): string {
  return createHash("sha256")
    .update(
      JSON.stringify({
        schemaVersion: INSTRUMENTED_BUILD_CACHE_SCHEMA_VERSION,
        executionFingerprint: integrity.fingerprint.execution,
        adapter: project.buildAdapter,
        command: project.buildCommand,
        environment: project.buildEnvironment,
        node: process.versions.node,
        platform: process.platform,
        architecture: process.arch,
      }),
    )
    .digest("hex");
}

export function readInstrumentedBuildCache(
  workspace: string,
  key: string,
): InstrumentedBuildCache | undefined {
  let metadata: InstrumentedBuildCache;
  try {
    metadata = JSON.parse(
      readFileSync(metadataPath(workspace), "utf8"),
    ) as InstrumentedBuildCache;
  } catch {
    return undefined;
  }
  if (
    metadata.schemaVersion !== INSTRUMENTED_BUILD_CACHE_SCHEMA_VERSION ||
    metadata.key !== key ||
    !Array.isArray(metadata.artifactPaths) ||
    metadata.artifactPaths.length === 0 ||
    metadata.artifactPaths.some(
      (path) => !safeRelativePath(path) || !existsSync(resolve(workspace, path)),
    )
  ) {
    return undefined;
  }
  return metadata;
}

export function writeInstrumentedBuildCache(
  workspace: string,
  key: string,
): InstrumentedBuildCache | undefined {
  let declared: { paths?: string[] } = {};
  try {
    declared = JSON.parse(
      readFileSync(resolve(workspace, ".supercov/build-outputs.json"), "utf8"),
    ) as { paths?: string[] };
  } catch {
    // Older/native Vite integrations use the conservative known outputs.
  }
  const existingOutputs = [
    ...new Set([
      ...BUILD_OUTPUT_CANDIDATES,
      ...(declared.paths ?? []).filter(safeRelativePath),
    ]),
  ].filter((path) =>
    existsSync(resolve(workspace, path)),
  );
  const outputs = existingOutputs.filter(
    (path) =>
      !existingOutputs.some(
        (parent) => parent !== path && path.startsWith(`${parent}/`),
      ),
  );
  const manifest = ".supercov/manifest.json";
  if (outputs.length === 0 || !existsSync(resolve(workspace, manifest)))
    return undefined;
  const metadata: InstrumentedBuildCache = {
    schemaVersion: INSTRUMENTED_BUILD_CACHE_SCHEMA_VERSION,
    key,
    createdAt: new Date().toISOString(),
    artifactPaths: [...outputs, manifest],
  };
  atomicWriteFileSync(
    metadataPath(workspace),
    `${JSON.stringify(metadata, null, 2)}\n`,
  );
  return metadata;
}

export function buildCacheReusePaths(
  metadata: InstrumentedBuildCache,
): string[] {
  return [...metadata.artifactPaths, ".supercov/build-cache.json"];
}
