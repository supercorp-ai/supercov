import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";
import { mcdcRuntimeModuleId, type InstrumentMcdcResult } from "./instrumenter.ts";
import { instrumentSource, instrumentSources } from "./engineInstrumenter.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import type {
  CoverageBranchMeta,
  CoverageLimitation,
  CoveragePointMeta,
  McdcDecisionMeta,
  CoverageManifest,
} from "./types.ts";

export interface McdcVitePluginOptions {
  root?: string;
  sourceRoots?: string[];
  sourceFiles?: string[];
  sourceScope?: CoverageManifest["scope"];
  sourceLimitations?: CoverageLimitation[];
  manifestPath?: string;
  buildOutputMetadataPath?: string;
}

export function mcdcVitePlugin(options: McdcVitePluginOptions = {}): Plugin {
  const root = options.root ?? process.cwd();
  const sourceRoots = (options.sourceRoots ?? ["app", "src"])
    .map((directory) => resolve(root, directory))
    .filter((directory) => existsSync(directory));
  const configuredSourceFiles = options.sourceFiles?.map((file) =>
    resolve(root, file)
  );
  const sourceFileSet = configuredSourceFiles
    ? new Set(configuredSourceFiles)
    : undefined;
  const runtimePath = fileURLToPath(new URL("./runtime.js", import.meta.url));
  const manifestPath = options.manifestPath
    ? resolve(root, options.manifestPath)
    : resolve(root, ".supercov/mcdc-manifest.json");
  const markerPath = resolve(dirname(manifestPath), ".mcdc-enabled");
  const buildOutputMetadataPath = options.buildOutputMetadataPath
    ? resolve(root, options.buildOutputMetadataPath)
    : undefined;
  const decisions = new Map<string, McdcDecisionMeta>();
  const points = new Map<string, CoveragePointMeta>();
  const branches = new Map<string, CoverageBranchMeta>();
  const limitations = new Map<string, CoverageLimitation>();
  const instrumentedById = new Map<
    string,
    { source: string; result: InstrumentMcdcResult }
  >();
  for (const limitation of options.sourceLimitations ?? [])
    limitations.set(limitation.id, limitation);

  const recordManifest = (manifest: {
    decisions: McdcDecisionMeta[];
    points: CoveragePointMeta[];
    branches: CoverageBranchMeta[];
    limitations?: CoverageLimitation[];
  }): void => {
    for (const decision of manifest.decisions)
      decisions.set(decision.id, decision);
    for (const point of manifest.points) points.set(point.id, point);
    for (const branch of manifest.branches) branches.set(branch.id, branch);
    for (const limitation of manifest.limitations ?? [])
      limitations.set(limitation.id, limitation);
  };

  const sourceFiles = (directory: string): string[] =>
    readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return sourceFiles(path);
      return /\.[cm]?[jt]sx?$/.test(entry.name) ? [path] : [];
    });

  return {
    name: "supercov-mcdc",
    enforce: "pre",
    configResolved(config) {
      if (!buildOutputMetadataPath) return;
      const configured = config.build.rollupOptions.output;
      const rollupOutputs = Array.isArray(configured)
        ? configured
        : configured
          ? [configured]
          : [];
      const absoluteOutputs = [
        config.build.outDir,
        ...rollupOutputs.flatMap((output) => [output.dir, output.file]),
      ].filter((value): value is string => Boolean(value));
      const owned = absoluteOutputs.flatMap((output) => {
        const local = relative(root, resolve(root, output));
        return local && local !== ".." && !local.startsWith(`..${sep}`)
          ? [local.split(sep).join("/")]
          : [];
      });
      let previous: { paths?: string[] } = {};
      try {
        previous = JSON.parse(
          readFileSync(buildOutputMetadataPath, "utf8"),
        ) as { paths?: string[] };
      } catch {
        // This is the first Vite configuration in the build.
      }
      atomicWriteFileSync(
        buildOutputMetadataPath,
        `${JSON.stringify({ paths: [...new Set([...(previous.paths ?? []), ...owned])].sort() }, null, 2)}\n`,
      );
    },
    resolveId(id) {
      if (id === mcdcRuntimeModuleId) return runtimePath;
      return null;
    },
    buildStart() {
      // Vite only transforms modules reachable from a build entry. Scan every
      // source file up front so never-imported executable code is still in the
      // denominator and appears as uncovered rather than disappearing.
      const inventory = configuredSourceFiles ?? sourceRoots.flatMap(sourceFiles);
      const pending = inventory.flatMap((id) => {
        if (!existsSync(id)) return [];
        return [{
          id,
          file: relative(root, id).split(sep).join("/"),
          source: readFileSync(id, "utf8"),
        }];
      });
      const results = instrumentSources(
        pending.map(({ file, source }) => ({ file, source })),
      );
      for (const [index, entry] of pending.entries()) {
        const result = results[index]!;
        instrumentedById.set(entry.id, { source: entry.source, result });
        recordManifest(result.manifest);
      }
    },
    transform(code, rawId) {
      const id = rawId.split("?")[0] ?? rawId;
      if (
        !(sourceFileSet
          ? sourceFileSet.has(id)
          : sourceRoots.some(
              (sourceRoot) =>
                id === sourceRoot || id.startsWith(`${sourceRoot}${sep}`),
            )) ||
        !/\.[cm]?[jt]sx?$/.test(id)
      )
        return null;
      const file = relative(root, id).split(sep).join("/");
      const cached = instrumentedById.get(id);
      const result = cached?.source === code
        ? cached.result
        : instrumentSource(code, file);
      recordManifest(result.manifest);
      return {
        code: result.code,
        ...(result.map ? { map: JSON.parse(JSON.stringify(result.map)) } : {}),
      };
    },
    closeBundle() {
      mkdirSync(dirname(manifestPath), { recursive: true });
      const sortByLocation = <
        T extends { file: string; line: number; column: number },
      >(
        values: T[],
      ): T[] =>
        values.sort((left, right) =>
          left.file === right.file
            ? left.line - right.line || left.column - right.column
            : left.file.localeCompare(right.file),
        );
      const manifest = {
        decisions: sortByLocation([...decisions.values()]),
        points: sortByLocation([...points.values()]),
        branches: sortByLocation([...branches.values()]),
        limitations: sortByLocation([...limitations.values()]),
        ...(options.sourceScope ? { scope: options.sourceScope } : {}),
      };
      atomicWriteFileSync(
        manifestPath,
        `${JSON.stringify(manifest, null, 2)}\n`,
      );
      atomicWriteFileSync(markerPath, "coverage-completeness-v2\n");
    },
  };
}
