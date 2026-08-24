import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
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
      return `const { ${properties} } = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? process.__SUPERCOV_DIRECT_RUNTIME__;`;
    },
  );
}

function moduleRuntime(code: string, sourcePath: string, runtimePath: string): string {
  const local = relative(resolve(sourcePath, ".."), runtimePath)
    .split(sep)
    .join("/");
  const specifier = /^(?:\.\.?\/)/.test(local) ? local : `./${local}`;
  return code.replaceAll(mcdcRuntimeModuleId, specifier);
}

function isInside(parent: string, child: string): boolean {
  const local = relative(parent, child);
  return local === "" || (!local.startsWith(`..${sep}`) && local !== "..");
}

function runtimeHostDirectory(candidate: string): string | undefined {
  // A source root may be a package entry target, i.e. a file such as
  // "lib/index.cjs"; the runtime then belongs beside it, not inside it.
  try {
    return statSync(candidate).isDirectory()
      ? candidate
      : resolve(candidate, "..");
  } catch {
    return undefined;
  }
}

function containedRuntimePath(
  root: string,
  sourcePath: string,
  sourceRoots: string[],
): string {
  const absoluteRoots = sourceRoots
    .map((sourceRoot) => resolve(root, sourceRoot))
    .filter((sourceRoot) => isInside(sourceRoot, sourcePath))
    .sort((left, right) => right.length - left.length);
  const sourceRoot = runtimeHostDirectory(absoluteRoots[0] ?? "");
  if (!sourceRoot || absoluteRoots.length === 0)
    return resolve(root, ".supercov/runtime.js");
  const runtimeDirectory = resolve(sourceRoot, ".supercov");
  mkdirSync(runtimeDirectory, { recursive: true });
  for (const file of ["runtime.js", "runtime.d.ts", "transport.js"]) {
    const generated = resolve(root, ".supercov", file);
    if (existsSync(generated)) copyFileSync(generated, resolve(runtimeDirectory, file));
  }
  return resolve(runtimeDirectory, "runtime.js");
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
  sourceFiles: string[],
  manifestPath: string,
  scope?: CoverageManifest["scope"],
  initialLimitations: CoverageLimitation[] = [],
  runtimeMode: "global" | "module" = "global",
  sourceRoots: string[] = [],
): CoverageManifest {
  const decisions: McdcDecisionMeta[] = [];
  const points: CoveragePointMeta[] = [];
  const branches: CoverageBranchMeta[] = [];
  const limitations: CoverageLimitation[] = [...initialLimitations];
  for (const sourceFile of sourceFiles) {
    const path = resolve(root, sourceFile);
    if (existsSync(path)) {
      const file = relative(root, path).split(sep).join("/");
      const result = instrumentMcdc(readFileSync(path, "utf8"), file);
      decisions.push(...result.manifest.decisions);
      points.push(...result.manifest.points);
      branches.push(...result.manifest.branches);
      limitations.push(...(result.manifest.limitations ?? []));
      const transformed =
        runtimeMode === "module"
          ? moduleRuntime(
              result.code,
              path,
              containedRuntimePath(root, path, sourceRoots),
            )
          : directRuntime(result.code);
      atomicWriteFileSync(
        path,
        /\.[cm]?tsx?$/.test(path)
          ? `// @ts-nocheck -- generated coverage workspace only\n${transformed}`
          : transformed,
      );
    }
  }
  const manifest: CoverageManifest = {
    decisions: sortByLocation(decisions),
    points: sortByLocation(points),
    branches: sortByLocation(branches),
    limitations: sortByLocation(limitations),
    ...(scope ? { scope } : {}),
  };
  atomicWriteFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}
