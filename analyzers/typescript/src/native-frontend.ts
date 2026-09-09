/** Adapter for the exact TypeScript 7.0.2 API, not a legacy compiler fallback. */
import type ts from "typescript";
import { createRequire, isBuiltin } from "node:module";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { analysisPath } from "./compiler.js";
import {
  selectedRootFiles,
  type CompilerFrontend,
  type SyntaxAPI,
} from "./frontend.js";

// The optional project dependency cannot be imported by the TS5 build. Native API
// objects are confined to this adapter; its public surface is the shared frontend.
type Native = any;

export function nativeFrontend(
  entry: string,
  projectRoot: string,
): CompilerFrontend {
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 12))
    throw new Error(
      "The TypeScript 7 frontend requires Node 22.12 or newer (Node 24 is tested)",
    );
  const require = createRequire(pathToFileURL(entry));
  const ast: Native = require("typescript/unstable/ast");
  const native: Native = require("typescript/unstable/sync");
  const root = analysisPath(projectRoot);
  const key = (file: string) => analysisPath(file).replaceAll("\\", "/");
  const virtual = new Map<string, string>();
  let parseSerial = 0;
  const opened = new Set<string>();
  const limitations = new Set<string>();
  let api: Native;
  let snapshot: Native;
  let closed = false;
  function client() {
    if (closed) throw new Error("TypeScript 7 frontend is closed");
    return (api ??= new native.API({
      cwd: root,
      fs: {
        readFile: (file: string) => virtual.get(key(file)),
        fileExists: (file: string) =>
          virtual.has(key(file)) ? true : undefined,
      },
    }));
  }
  function update(config: string) {
    const previous = snapshot;
    snapshot = client().updateSnapshot({
      ...(!opened.has(config) ? { openProjects: [config] } : {}),
      fileChanges: { invalidateAll: true },
    });
    opened.add(config);
    previous?.dispose();
    const project = snapshot.getProject(config);
    if (!project)
      throw new Error(
        `TypeScript 7 did not open the analysis project: ${config}`,
      );
    return project;
  }
  function overlayConfig(path: string, config: unknown) {
    if (existsSync(path))
      throw new Error(`Reserved analysis config already exists: ${path}`);
    virtual.set(key(path), JSON.stringify(config));
  }
  const kinds = ast.SyntaxKind;
  const functionKinds = new Set(
    [
      "FunctionDeclaration",
      "MethodDeclaration",
      "Constructor",
      "GetAccessor",
      "SetAccessor",
      "FunctionExpression",
      "ArrowFunction",
      "MethodSignature",
      "CallSignature",
      "JSDocSignature",
      "ConstructSignature",
      "IndexSignature",
      "FunctionType",
      "JSDocFunctionType",
      "ConstructorType",
    ]
      .map((name) => kinds[name])
      .filter((value) => typeof value === "number"),
  );
  const loopKinds = new Set(
    [
      "ForStatement",
      "ForInStatement",
      "ForOfStatement",
      "WhileStatement",
      "DoStatement",
    ].map((name) => kinds[name]),
  );
  // These adapters preserve native nodes and native enum values. No TS5 predicate
  // may inspect a TS7 node: syntax-kind numeric values are different.
  const syntax = {
    ...ast,
    SymbolFlags: native.SymbolFlags,
    forEachChild: (node: Native, visit: Native, visitArray?: Native) =>
      node.forEachChild(visit, visitArray),
    isFunctionLike: (node: Native) => !!node && functionKinds.has(node.kind),
    isIterationStatement: (
      node: Native,
      lookInLabeledStatements: boolean,
    ): boolean =>
      !!node &&
      (loopKinds.has(node.kind) ||
        (lookInLabeledStatements &&
          node.kind === kinds.LabeledStatement &&
          syntax.isIterationStatement(node.statement, true))),
    isMethodSignature: ast.isMethodSignatureDeclaration,
    isParameter: ast.isParameterDeclaration,
    isTypeAssertionExpression: ast.isTypeAssertion,
    isStringLiteralLike: (node: Native) =>
      !!node &&
      (node.kind === kinds.StringLiteral ||
        node.kind === kinds.NoSubstitutionTemplateLiteral),
    canHaveModifiers: (node: Native) => !!node && "modifiers" in node,
    getModifiers: (node: Native) =>
      node.modifiers?.filter((m: Native) => m.kind !== kinds.Decorator),
  } as unknown as SyntaxAPI;
  for (const name of [
    "isMethodSignature",
    "isParameter",
    "isTypeAssertionExpression",
    "getLeadingCommentRanges",
    "getTrailingCommentRanges",
  ] as const)
    if (typeof syntax[name] !== "function")
      throw new Error(`Unsupported TypeScript 7 syntax API: ${name}`);

  const frontend: CompilerFrontend = {
    kind: "typescript-native-7",
    version: "7.0.2",
    syntax,
    limitations,
    close() {
      if (closed) return;
      closed = true;
      try {
        snapshot?.dispose();
      } finally {
        api?.close();
      }
    },
    parseSource(file, text) {
      const absolute = resolve(root, file);
      virtual.set(key(absolute), text);
      // Do not rely on 7.0.2 reloading an edited virtual config's root-file list.
      const parseConfig = resolve(
        root,
        `.supercov-asserted-parse-${++parseSerial}.tsconfig.json`,
      );
      overlayConfig(parseConfig, {
        files: [absolute],
        compilerOptions: {
          allowJs: true,
          checkJs: true,
          noResolve: true,
          noLib: true,
          noEmit: true,
          types: [],
        },
      });
      const project = update(parseConfig);
      const source = project.program.getSourceFile(absolute);
      if (!source || source.text !== text)
        throw new Error(`Native parser source mismatch: ${file}`);
      const errors = project.program.getSyntacticDiagnostics(absolute);
      if (errors.length)
        throw new Error(
          `Native parser rejected ${file}: ${JSON.stringify(errors)}`,
        );
      return source as ts.SourceFile;
    },
    openProgram(options) {
      const configPath = resolve(root, options.tsconfig ?? "tsconfig.json");
      const hasConfig = existsSync(configPath);
      if (!hasConfig && !options.sourceFiles)
        throw new Error(`Missing TypeScript config: ${configPath}`);
      const configured = hasConfig
        ? client().parseConfigFile(configPath).fileNames
        : [];
      const files = selectedRootFiles(options, configured);
      const overlay = resolve(
        dirname(configPath),
        ".supercov-asserted-query.tsconfig.json",
      );
      overlayConfig(overlay, {
        ...(hasConfig ? { extends: configPath } : {}),
        files,
        include: [],
        exclude: [],
        compilerOptions: {
          ...(options.sourceFiles ? { allowJs: true, checkJs: true } : {}),
          noEmit: true,
        },
      });
      const project = update(overlay);
      const program = project.program;
      const configErrors = program.getConfigFileParsingDiagnostics();
      if (configErrors.length)
        throw new Error(
          `TypeScript 7 configuration is invalid: ${JSON.stringify(configErrors)}`,
        );
      const checker = project.checker;
      const symbols = new WeakMap<object, ts.Symbol>();
      const nativeSymbols = new WeakMap<object, Native>();
      function wrapSymbol(symbol: Native): ts.Symbol | undefined {
        if (!symbol) return undefined;
        let wrapped = symbols.get(symbol);
        if (!wrapped) {
          wrapped = {
            flags: symbol.flags,
            name: symbol.name,
            escapedName: symbol.escapedName,
            get valueDeclaration() {
              return symbol.valueDeclaration?.resolve(project);
            },
            get declarations() {
              return symbol.declarations.map((handle: Native) => {
                const node = handle.resolve(project);
                if (!node)
                  throw new Error(
                    "TypeScript 7 declaration handle did not resolve",
                  );
                return node;
              });
            },
          } as ts.Symbol;
          symbols.set(symbol, wrapped);
          nativeSymbols.set(wrapped, symbol);
        }
        return wrapped;
      }
      const allFiles: ts.SourceFile[] = program
        .getSourceFileNames()
        .map((file: string) => {
          const source = program.getSourceFile(file);
          if (!source) throw new Error(`Native source disappeared: ${file}`);
          return source;
        });
      const resolutionCache = new Map<string, string | undefined>();
      return {
        files: allFiles,
        checker: {
          getSymbolAtLocation: (node) =>
            wrapSymbol(checker.getSymbolAtLocation(node)),
          getShorthandAssignmentValueSymbol: (node) =>
            wrapSymbol(checker.getShorthandAssignmentValueSymbol(node)),
          getAliasedSymbol: (symbol) => {
            const original = nativeSymbols.get(symbol);
            if (!original)
              throw new Error("Symbol belongs to a different compiler session");
            return wrapSymbol(checker.getAliasedSymbol(original))!;
          },
          getResolvedSignature: (node) => {
            const signature = checker.getResolvedSignature(node);
            return signature
              ? ({
                  getParameters: () =>
                    signature.getParameters().map(wrapSymbol),
                } as ts.Signature)
              : undefined;
          },
        },
        resolveModule(specifier, from) {
          const cacheKey = `${key(from)}\0${specifier}`;
          if (resolutionCache.has(cacheKey))
            return resolutionCache.get(cacheKey);
          const source = program.getSourceFile(from);
          const paths = new Set<string>();
          const visit = (node: Native) => {
            if (syntax.isStringLiteralLike(node) && node.text === specifier) {
              // Ask the native checker about actual module-reference syntax. A
              // vi.mock string is not a TypeScript import and is not guessed.
              const p: Native = node.parent;
              if (
                p &&
                (ast.isImportDeclaration(p) ||
                  ast.isExportDeclaration(p) ||
                  (ast.isCallExpression(p) &&
                    p.expression.kind === kinds.ImportKeyword))
              ) {
                const symbol = checker.getSymbolAtLocation(node);
                for (const handle of symbol?.declarations ?? []) {
                  const declaration = handle.resolve(project);
                  if (declaration && syntax.isSourceFile(declaration))
                    paths.add(declaration.fileName);
                }
              }
            }
            node.forEachChild(visit);
          };
          if (source) visit(source);
          const result = paths.size === 1 ? [...paths][0] : undefined;
          if (!result && !isBuiltin(specifier))
            limitations.add(
              `native-module-resolution: ${key(from)} → ${specifier}`,
            );
          resolutionCache.set(cacheKey, result);
          return result;
        },
      };
    },
  };
  return frontend;
}
