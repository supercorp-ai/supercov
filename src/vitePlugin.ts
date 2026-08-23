import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";
import { instrumentMcdc, mcdcRuntimeModuleId } from "./instrumenter.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import type {
  CoverageBranchMeta,
  CoverageLimitation,
  CoveragePointMeta,
  McdcDecisionMeta,
} from "./types.ts";

export interface McdcVitePluginOptions {
  root?: string;
  sourceRoots?: string[];
  manifestPath?: string;
}

export function mcdcVitePlugin(options: McdcVitePluginOptions = {}): Plugin {
  const root = options.root ?? process.cwd();
  const sourceRoots = (options.sourceRoots ?? ["app", "src"])
    .map((directory) => resolve(root, directory))
    .filter((directory) => existsSync(directory));
  const runtimePath = fileURLToPath(new URL("./runtime.js", import.meta.url));
  const manifestPath = options.manifestPath
    ? resolve(root, options.manifestPath)
    : resolve(root, ".supercov/mcdc-manifest.json");
  const markerPath = resolve(dirname(manifestPath), ".mcdc-enabled");
  const decisions = new Map<string, McdcDecisionMeta>();
  const points = new Map<string, CoveragePointMeta>();
  const branches = new Map<string, CoverageBranchMeta>();
  const limitations = new Map<string, CoverageLimitation>();

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
    resolveId(id) {
      if (id === mcdcRuntimeModuleId) return runtimePath;
      return null;
    },
    buildStart() {
      // Vite only transforms modules reachable from a build entry. Scan every
      // source file up front so never-imported executable code is still in the
      // denominator and appears as uncovered rather than disappearing.
      for (const sourceRoot of sourceRoots) {
        for (const id of sourceFiles(sourceRoot)) {
          const file = relative(root, id).split(sep).join("/");
          recordManifest(
            instrumentMcdc(readFileSync(id, "utf8"), file).manifest,
          );
        }
      }
    },
    transform(code, rawId) {
      const id = rawId.split("?")[0] ?? rawId;
      if (
        !sourceRoots.some(
          (sourceRoot) =>
            id === sourceRoot || id.startsWith(`${sourceRoot}${sep}`),
        ) ||
        !/\.[cm]?[jt]sx?$/.test(id)
      )
        return null;
      const file = relative(root, id).split(sep).join("/");
      const result = instrumentMcdc(code, file);
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
      };
      atomicWriteFileSync(
        manifestPath,
        `${JSON.stringify(manifest, null, 2)}\n`,
      );
      atomicWriteFileSync(markerPath, "coverage-completeness-v2\n");
    },
  };
}
