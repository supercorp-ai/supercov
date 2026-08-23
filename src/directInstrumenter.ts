import {
  existsSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import { atomicWriteFileSync } from "./atomic.ts";
import { instrumentMcdc, mcdcRuntimeModuleId } from "./instrumenter.ts";
import type {
  CoverageBranchMeta,
  CoverageLimitation,
  CoverageManifest,
  CoveragePointMeta,
  McdcDecisionMeta,
} from "./types.ts";

function sourceFiles(directory: string): string[] {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return entry.isFile() && /\.[cm]?[jt]sx?$/.test(entry.name) &&
      !/\.d\.[cm]?ts$/.test(entry.name)
      ? [path]
      : [];
  });
}

function directRuntime(code: string): string {
  const escapedModule = mcdcRuntimeModuleId.replace(
    /[.*+?^${}()|[\]\\]/g,
    "\\$&",
  );
  return code.replace(
    new RegExp(
      `import\\s*\\{([\\s\\S]*?)\\}\\s*from\\s*["']${escapedModule}["'];?`,
    ),
    (_statement, bindings: string) => {
      const properties = bindings
        .split(",")
        .map((binding) => binding.trim())
        .filter(Boolean)
        .map((binding) => {
          const [imported = "", local = imported] = binding
            .split(/\s+as\s+/)
            .map((value) => value.trim());
          return imported === local ? imported : `${imported}: ${local}`;
        })
        .join(", ");
      return `const { ${properties} } = globalThis.__SUPERCOV_DIRECT_RUNTIME__;`;
    },
  );
}

function sortByLocation<
  T extends { file: string; line: number; column: number },
>(values: T[]): T[] {
  return values.sort((left, right) =>
    left.file === right.file
      ? left.line - right.line || left.column - right.column
      : left.file.localeCompare(right.file),
  );
}

/**
 * Instrument source inside Supercov's disposable workspace when a project has
 * no build step. The preload supplies the runtime through a global so the
 * transformed source remains valid in ESM, CommonJS, and transpiler inputs.
 */
export function instrumentDirectWorkspace(
  root: string,
  sourceRoots: string[],
  manifestPath: string,
): CoverageManifest {
  const decisions: McdcDecisionMeta[] = [];
  const points: CoveragePointMeta[] = [];
  const branches: CoverageBranchMeta[] = [];
  const limitations: CoverageLimitation[] = [];
  for (const sourceRoot of sourceRoots) {
    for (const path of sourceFiles(resolve(root, sourceRoot))) {
      const file = relative(root, path).split(sep).join("/");
      const result = instrumentMcdc(readFileSync(path, "utf8"), file);
      decisions.push(...result.manifest.decisions);
      points.push(...result.manifest.points);
      branches.push(...result.manifest.branches);
      limitations.push(...(result.manifest.limitations ?? []));
      atomicWriteFileSync(path, directRuntime(result.code));
    }
  }
  const manifest: CoverageManifest = {
    decisions: sortByLocation(decisions),
    points: sortByLocation(points),
    branches: sortByLocation(branches),
    limitations: sortByLocation(limitations),
  };
  atomicWriteFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}
