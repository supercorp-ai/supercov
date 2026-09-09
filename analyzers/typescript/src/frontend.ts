/** Compiler-specific work stays here; assertion/flow rules do not select a compiler. */
import type ts from "typescript";
import { existsSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import {
  analysisPath,
  loadProjectCompiler,
  projectCompilerPath,
} from "./compiler.js";
import { nativeFrontend } from "./native-frontend.js";
import type { AnalyzeOptions } from "./types.js";

// Only syntax predicates, traversal, comments and enums cross this boundary.
// Program creation, resolution and symbol handles have separate adapters.
export type SyntaxAPI = Pick<
  typeof ts,
  | Extract<keyof typeof ts, `is${string}`>
  | "SyntaxKind"
  | "NodeFlags"
  | "SymbolFlags"
  | "forEachChild"
  | "canHaveModifiers"
  | "getModifiers"
  | "getLeadingCommentRanges"
  | "getTrailingCommentRanges"
>;
export type CheckerAPI = Pick<
  ts.TypeChecker,
  | "getSymbolAtLocation"
  | "getShorthandAssignmentValueSymbol"
  | "getAliasedSymbol"
  | "getResolvedSignature"
>;
export interface AnalysisProgram {
  files: ts.SourceFile[];
  checker: CheckerAPI;
  resolveModule(specifier: string, from: string): string | undefined;
}
export interface CompilerFrontend {
  kind: "typescript-legacy" | "typescript-native-7";
  version: string;
  syntax: SyntaxAPI;
  limitations: Set<string>;
  openProgram(options: AnalyzeOptions): AnalysisProgram;
  parseSource(file: string, text: string): ts.SourceFile;
  close(): void;
}

export function selectedRootFiles(
  options: AnalyzeOptions,
  configured: string[],
): string[] {
  const root = analysisPath(options.projectRoot);
  if (options.sourceFiles)
    return [...options.sourceFiles, ...(options.testFiles ?? [])].map((f) =>
      resolve(root, f),
    );
  if (!options.sourceDir) return configured;
  function walk(dir: string): string[] {
    if (!existsSync(dir)) return [];
    return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
      if (e.name === "node_modules" || e.name.startsWith(".")) return [];
      const file = resolve(dir, e.name);
      return e.isDirectory()
        ? walk(file)
        : /\.(ts|tsx|mts)$/.test(e.name) && !e.name.endsWith(".d.ts")
          ? [file]
          : [];
    });
  }
  return [
    ...walk(resolve(root, options.sourceDir)),
    ...walk(resolve(root, options.testDir ?? "tests")),
    ...(existsSync(resolve(root, "env.d.ts"))
      ? [resolve(root, "env.d.ts")]
      : []),
  ];
}

function legacyFrontend(compiler: typeof ts): CompilerFrontend {
  return {
    kind: "typescript-legacy",
    version: compiler.version,
    syntax: compiler,
    limitations: new Set(),
    close() {},
    openProgram(options) {
      const root = analysisPath(options.projectRoot);
      const configPath = resolve(root, options.tsconfig ?? "tsconfig.json");
      const cfg =
        options.sourceFiles && !existsSync(configPath)
          ? { config: { compilerOptions: { allowJs: true, checkJs: true } } }
          : compiler.readConfigFile(configPath, compiler.sys.readFile);
      if (cfg.error)
        throw new Error(
          compiler.flattenDiagnosticMessageText(cfg.error.messageText, "\n"),
        );
      const parsed = compiler.parseJsonConfigFileContent(
        cfg.config,
        compiler.sys,
        dirname(configPath),
      );
      const program = compiler.createProgram(
        selectedRootFiles(options, parsed.fileNames),
        {
          ...parsed.options,
          ...(options.sourceFiles ? { allowJs: true, checkJs: true } : {}),
          noEmit: true,
        },
      );
      return {
        files: program.getSourceFiles() as ts.SourceFile[],
        checker: program.getTypeChecker(),
        resolveModule: (spec, from) =>
          compiler.resolveModuleName(spec, from, parsed.options, compiler.sys)
            .resolvedModule?.resolvedFileName,
      };
    },
    parseSource: (file, text) =>
      compiler.createSourceFile(file, text, compiler.ScriptTarget.Latest, true),
  };
}

export function createFrontend(
  root: string,
  supplied?: typeof ts,
): CompilerFrontend {
  if (supplied) return legacyFrontend(supplied);
  const entry = projectCompilerPath(root);
  const require = createRequire(pathToFileURL(entry));
  const version = require(entry).version;
  if (version === "7.0.2") return nativeFrontend(entry, root);
  return legacyFrontend(loadProjectCompiler(root));
}
