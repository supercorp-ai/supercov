import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import generate from "@babel/generator";
import { parse, type ParserPlugin } from "@babel/parser";
import traverse, { type NodePath } from "@babel/traverse";
import * as t from "@babel/types";
import { atomicWriteFileSync } from "./atomic.ts";

const ASSERT_MODULES = new Set([
  "assert",
  "assert/strict",
  "node:assert",
  "node:assert/strict",
]);
const ASSERTION_EXPORTS = new Set([
  "deepEqual",
  "deepStrictEqual",
  "doesNotMatch",
  "doesNotReject",
  "doesNotThrow",
  "equal",
  "fail",
  "ifError",
  "match",
  "notDeepEqual",
  "notDeepStrictEqual",
  "notEqual",
  "notStrictEqual",
  "ok",
  "partialDeepStrictEqual",
  "rejects",
  "strictEqual",
  "throws",
]);

function moduleName(node: t.Node | null | undefined): string | undefined {
  return t.isStringLiteral(node) && ASSERT_MODULES.has(node.value)
    ? node.value.startsWith("node:")
      ? node.value
      : `node:${node.value}`
    : undefined;
}

function requiredModule(node: t.Node | null | undefined): string | undefined {
  if (
    !t.isCallExpression(node) ||
    !t.isIdentifier(node.callee, { name: "require" }) ||
    node.arguments.length !== 1
  )
    return undefined;
  return moduleName(node.arguments[0] as t.Node);
}

function containsAwaitOrYield(path: NodePath<t.CallExpression>): boolean {
  let found = false;
  path.traverse({
    Function(inner) {
      inner.skip();
    },
    AwaitExpression(inner) {
      found = true;
      inner.stop();
    },
    YieldExpression(inner) {
      found = true;
      inner.stop();
    },
  });
  return found;
}

function memberNames(node: t.MemberExpression): string[] | undefined {
  const names: string[] = [];
  let current: t.Expression | t.Super = node;
  while (t.isMemberExpression(current)) {
    const property = current.computed
      ? t.isStringLiteral(current.property)
        ? current.property.value
        : undefined
      : t.isIdentifier(current.property)
        ? current.property.name
        : undefined;
    if (!property) return undefined;
    names.unshift(property);
    current = current.object;
  }
  return t.isIdentifier(current) ? [current.name, ...names] : undefined;
}

export interface NodeAssertionInstrumentationResult {
  code: string;
  assertions: number;
}

/** Wrap native node:assert calls before their arguments are evaluated. */
export function instrumentNodeAssertionPhases(
  code: string,
  file: string,
  extraExpectModules: string[] = [],
): NodeAssertionInstrumentationResult {
  const mayUseNodeAssert = /(?:node:)?assert(?:\/strict)?["']/.test(code);
  const mayUseContextualExpect =
    /["']node:test["']/.test(code) && /\bexpect\b/.test(code);
  const lexicalExpectModules = new Set(["vitest", ...extraExpectModules]);
  const mayUseLexicalExpect =
    /\bexpect\b/.test(code) &&
    [...lexicalExpectModules].some(
      (module) => code.includes(`"${module}"`) || code.includes(`'${module}'`),
    );
  const mayUseNodeExpect = mayUseContextualExpect || mayUseLexicalExpect;
  if (!mayUseNodeAssert && !mayUseNodeExpect)
    return { code, assertions: 0 };
  const plugins: ParserPlugin[] = [
    "typescript",
    "decorators-legacy",
    "sourcePhaseImports",
    ...(file.endsWith("x") ? (["jsx"] as const) : []),
  ];
  const ast = parse(code, {
    sourceType: "unambiguous",
    sourceFilename: file,
    errorRecovery: true,
    plugins,
  });
  const objectBindings = new Map<string, string>();
  const directBindings = new Map<string, string>();
  const expectBindings = new Set<string>();

  traverse(ast, {
    ImportDeclaration(path) {
      const importedModule = moduleName(path.node.source);
      if (!importedModule) {
        if (!mayUseNodeExpect) return;
        if (
          !mayUseContextualExpect &&
          !(
            t.isStringLiteral(path.node.source) &&
            lexicalExpectModules.has(path.node.source.value)
          )
        )
          return;
        for (const specifier of path.node.specifiers) {
          if (
            t.isImportSpecifier(specifier) &&
            ((t.isIdentifier(specifier.imported) && specifier.imported.name === "expect") ||
              (t.isStringLiteral(specifier.imported) && specifier.imported.value === "expect"))
          )
            expectBindings.add(specifier.local.name);
          else if (
            t.isImportDefaultSpecifier(specifier) &&
            specifier.local.name === "expect"
          )
            expectBindings.add(specifier.local.name);
        }
        return;
      }
      for (const specifier of path.node.specifiers) {
        if (t.isImportDefaultSpecifier(specifier) || t.isImportNamespaceSpecifier(specifier)) {
          objectBindings.set(specifier.local.name, importedModule);
          continue;
        }
        const imported = t.isIdentifier(specifier.imported)
          ? specifier.imported.name
          : specifier.imported.value;
        if (imported === "strict")
          objectBindings.set(specifier.local.name, "node:assert/strict");
        else if (ASSERTION_EXPORTS.has(imported))
          directBindings.set(
            specifier.local.name,
            `${importedModule}.${imported}`,
          );
      }
    },
    VariableDeclarator(path) {
      let importedModule = requiredModule(path.node.init);
      if (
        !importedModule &&
        t.isMemberExpression(path.node.init) &&
        !path.node.init.computed &&
        t.isIdentifier(path.node.init.property, { name: "strict" })
      ) {
        const base = requiredModule(path.node.init.object);
        if (base) importedModule = "node:assert/strict";
      }
      if (!importedModule) return;
      if (t.isIdentifier(path.node.id)) {
        objectBindings.set(path.node.id.name, importedModule);
        return;
      }
      if (!t.isObjectPattern(path.node.id)) return;
      for (const property of path.node.id.properties) {
        if (
          !t.isObjectProperty(property) ||
          !t.isIdentifier(property.value)
        )
          continue;
        const imported = t.isIdentifier(property.key)
          ? property.key.name
          : t.isStringLiteral(property.key)
            ? property.key.value
            : undefined;
        if (!imported) continue;
        if (imported === "strict")
          objectBindings.set(property.value.name, "node:assert/strict");
        else if (ASSERTION_EXPORTS.has(imported))
          directBindings.set(
            property.value.name,
            `${importedModule}.${imported}`,
          );
      }
    },
  });

  if (
    objectBindings.size === 0 &&
    directBindings.size === 0 &&
    expectBindings.size === 0
  )
    return { code, assertions: 0 };
  let assertions = 0;
  let phaseBinding: t.Identifier | undefined;
  traverse(ast, {
    CallExpression(path) {
      if (containsAwaitOrYield(path)) return;
      let operation: string | undefined;
      if (t.isIdentifier(path.node.callee)) {
        operation = directBindings.get(path.node.callee.name);
        const importedModule = objectBindings.get(path.node.callee.name);
        if (importedModule) operation = `${importedModule}.ok`;
      } else if (t.isMemberExpression(path.node.callee)) {
        const names = memberNames(path.node.callee);
        const importedModule = names ? objectBindings.get(names[0]!) : undefined;
        const method = names?.at(-1);
        if (
          importedModule &&
          method &&
          ASSERTION_EXPORTS.has(method)
        ) {
          const strict = names?.slice(1, -1).includes("strict");
          operation = `${strict ? "node:assert/strict" : importedModule}.${method}`;
        }
        if (!operation) {
          const matcherNames: string[] = [];
          let current: t.Expression | t.Super = path.node.callee;
          while (t.isMemberExpression(current)) {
            const property = current.computed
              ? t.isStringLiteral(current.property)
                ? current.property.value
                : undefined
              : t.isIdentifier(current.property)
                ? current.property.name
                : undefined;
            if (!property) break;
            matcherNames.unshift(property);
            current = current.object;
          }
          if (
            t.isCallExpression(current) &&
            t.isIdentifier(current.callee) &&
            expectBindings.has(current.callee.name) &&
            /^to[A-Z]/.test(matcherNames.at(-1) ?? "")
          )
            operation = `expect.${matcherNames.join(".")}`;
        }
      }
      if (!operation || !path.node.loc) return;
      phaseBinding ??= path.scope.getProgramParent().generateUidIdentifier(
        "supercovNodeAssertion",
      );
      const original = path.node;
      const source = `${file}:${path.node.loc.start.line}:${path.node.loc.start.column + 1}`;
      path.replaceWith(
        t.callExpression(t.cloneNode(phaseBinding), [
          t.stringLiteral(operation),
          t.stringLiteral(source),
          t.arrowFunctionExpression([], original),
        ]),
      );
      assertions += 1;
      path.skip();
    },
  });
  if (!phaseBinding || assertions === 0) return { code, assertions: 0 };
  ast.program.body.unshift(
    t.variableDeclaration("const", [
      t.variableDeclarator(
        phaseBinding,
        t.memberExpression(
          t.memberExpression(
            t.identifier("globalThis"),
            t.stringLiteral("__SUPERCOV_DIRECT_RUNTIME__"),
            true,
          ),
          t.identifier("withNodeAssertionPhase"),
        ),
      ),
    ]),
  );
  const output = generate(
    ast,
    {
      retainLines: true,
      comments: true,
      sourceMaps: false,
      sourceFileName: file,
    },
    code,
  );
  return { code: output.code, assertions };
}

export function instrumentNodeAssertionsInWorkspace(
  root: string,
  files: string[],
  extraExpectModules: string[] = [],
): number {
  let assertions = 0;
  for (const file of files) {
    const path = resolve(root, file);
    if (!existsSync(path)) continue;
    const code = readFileSync(path, "utf8");
    const transformed = instrumentNodeAssertionPhases(
      code,
      file,
      extraExpectModules,
    );
    if (transformed.assertions === 0) continue;
    atomicWriteFileSync(path, transformed.code);
    assertions += transformed.assertions;
  }
  return assertions;
}
