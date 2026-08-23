import generate from "@babel/generator";
import { parse, type ParserPlugin } from "@babel/parser";
import traverse from "@babel/traverse";
import * as t from "@babel/types";

const CAPABILITY_PATTERN =
  /\b(?:hostPath|guestPath|mounts?|snapshot(?:Key|Id|Tag)?|createPool|acquire|container|machine|sandbox)\b|\.(?:exec|execute|launch|spawn)\s*\(/;
const EXCLUDED_IMPORT = /^(?:node:|@playwright\/test$|playwright$|vitest$|@jest\/globals$)/;

function parserPlugins(url: string): ParserPlugin[] {
  const plugins: ParserPlugin[] = [
    "decorators-legacy",
    "sourcePhaseImports",
  ];
  if (/\.[cm]?tsx?(?:\?|$)/.test(url)) plugins.push("typescript");
  if (/\.[jt]sx(?:\?|$)/.test(url)) plugins.push("jsx");
  return plugins;
}

/** Wrap imported SDK values only in source that exhibits remote-capability shapes. */
export function transformCapabilityImports(
  source: string,
  url: string,
  wrapperUrl: string,
): { code: string; transformed: boolean } {
  if (!CAPABILITY_PATTERN.test(source)) return { code: source, transformed: false };
  const ast = parse(source, {
    sourceType: "module",
    sourceFilename: url,
    plugins: parserPlugins(url),
  });
  let transformed = false;
  let wrapperId: t.Identifier | undefined;
  traverse(ast, {
    ImportDeclaration(path) {
      const specifier = path.node.source.value;
      if (
        path.node.importKind === "type" ||
        EXCLUDED_IMPORT.test(specifier) ||
        specifier === wrapperUrl ||
        path.node.specifiers.length === 0
      ) return;
      const declarations: t.VariableDeclarator[] = [];
      for (const imported of path.node.specifiers) {
        if (t.isImportSpecifier(imported) && imported.importKind === "type") continue;
        const original = t.cloneNode(imported.local);
        const raw = path.scope.generateUidIdentifier(`${original.name}SupercovRaw`);
        imported.local = raw;
        wrapperId ??= path.scope.generateUidIdentifier("supercovImportedCapability");
        declarations.push(
          t.variableDeclarator(
            original,
            t.callExpression(t.cloneNode(wrapperId), [t.cloneNode(raw)]),
          ),
        );
      }
      if (declarations.length > 0) {
        path.insertAfter(t.variableDeclaration("const", declarations));
        transformed = true;
      }
    },
  });
  if (!transformed || !wrapperId) return { code: source, transformed: false };
  ast.program.body.unshift(
    t.importDeclaration(
      [
        t.importSpecifier(
          t.cloneNode(wrapperId),
          t.identifier("wrapImportedCapability"),
        ),
      ],
      t.stringLiteral(wrapperUrl),
    ),
  );
  return {
    code: generate(ast, { retainLines: true, comments: true }, source).code,
    transformed: true,
  };
}
