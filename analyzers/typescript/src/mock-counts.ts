/** A bounded synchronous source model, not general execution or value-flow analysis. */
import type ts from "typescript";
import type { SyntaxAPI } from "./frontend.js";

export interface MockCountEvidence {
  model: "node-sync-console-count-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  instance?: string;
  createdAt?: string;
  resetAt?: string;
  readAt?: string;
  installedAtRead?: boolean;
  expectedCount?: number;
  observedCount?: number;
  calls?: { source: string; action: string; site?: string }[];
}

interface Model {
  declaration(node: ts.Node): ts.Declaration | undefined;
  location(node: ts.Node): string;
  nativeMock(call: ts.CallExpression): boolean;
  nativePredicate(call: ts.CallExpression): string | undefined;
  globalConsole(expr: ts.Expression): boolean;
  production(node: ts.Node): boolean;
  site(call: ts.CallExpression): string | undefined;
}

export function analyzeMockCounts(
  syntax: SyntaxAPI,
  fn: ts.Node,
  model: Model,
) {
  const ts = syntax;
  type Primitive = string | number | boolean | null | undefined;
  type Call = NonNullable<MockCountEvidence["calls"]>[number];
  type Mock = {
    kind: "mock";
    target: string;
    source: string;
    previous?: Mock;
    resetAt?: string;
    calls: Call[];
  };
  type Snapshot = { kind: "count" | "history"; evidence: MockCountEvidence };
  type Value = Primitive | Mock | Snapshot | { kind: "context"; mock: Mock };
  const checks = new Map<ts.CallExpression, MockCountEvidence>();
  const installed = new Map<string, Mock | undefined>();
  const locals = new Map<ts.Declaration, Value>();
  const moduleChecks = new Map<ts.SourceFile, boolean>();
  const targetChecks = new Map<ts.Declaration, boolean>();
  let budget = 4096;
  class Unsupported extends Error {}
  const fail = (node: ts.Node, why: string): never => {
    throw new Unsupported(`${why} at ${model.location(node)}`);
  };
  const primitive = (v: Value): v is Primitive =>
    v === null || typeof v !== "object";
  const peel = (expr: ts.Expression): ts.Expression => {
    while (
      ts.isParenthesizedExpression(expr) ||
      ts.isAsExpression(expr) ||
      ts.isTypeAssertionExpression(expr) ||
      ts.isNonNullExpression(expr) ||
      ts.isSatisfiesExpression(expr)
    )
      expr = expr.expression;
    return expr;
  };
  const snapshot = (
    mock: Mock,
    node: ts.Node,
    kind: Snapshot["kind"],
  ): Snapshot => ({
    kind,
    evidence: {
      model: "node-sync-console-count-v1",
      status: "source-checked",
      instance: mock.source,
      createdAt: mock.source,
      resetAt: mock.resetAt,
      readAt: model.location(node),
      installedAtRead: installed.get(mock.target) === mock,
      observedCount: mock.calls.length,
      calls: mock.calls.map((call) => ({ ...call })),
    },
  });
  const record = (
    mock: Mock | undefined,
    node: ts.CallExpression,
    action: string,
  ) => {
    if (mock)
      mock.calls.push({
        source: model.location(node),
        action,
        site: model.site(node),
      });
  };

  function stableTarget(declaration: ts.Declaration): boolean {
    const cached = targetChecks.get(declaration);
    if (cached !== undefined) return cached;
    const sf = declaration.getSourceFile();
    let safeModule = moduleChecks.get(sf);
    if (safeModule === undefined) {
      const inertInitializer = (e: ts.Expression) => {
        const n = peel(e);
        return (
          ts.isArrowFunction(n) ||
          ts.isFunctionExpression(n) ||
          ts.isStringLiteralLike(n) ||
          ts.isNumericLiteral(n) ||
          [
            ts.SyntaxKind.TrueKeyword,
            ts.SyntaxKind.FalseKeyword,
            ts.SyntaxKind.NullKeyword,
          ].includes(n.kind)
        );
      };
      safeModule = sf.statements.every(
        (s) =>
          ts.isImportDeclaration(s) ||
          ts.isExportDeclaration(s) ||
          ts.isFunctionDeclaration(s) ||
          ts.isInterfaceDeclaration(s) ||
          ts.isTypeAliasDeclaration(s) ||
          ts.isEmptyStatement(s) ||
          (ts.isVariableStatement(s) &&
            !!(s.declarationList.flags & ts.NodeFlags.Const) &&
            s.declarationList.declarations.every(
              (d) =>
                ts.isIdentifier(d.name) &&
                !!d.initializer &&
                inertInitializer(d.initializer),
            )),
      );
      moduleChecks.set(sf, safeModule);
    }
    if (!safeModule) {
      targetChecks.set(declaration, false);
      return false;
    }
    let stable = true;
    const writesTarget = (node: ts.Node) => {
      if (ts.isIdentifier(node) && model.declaration(node) === declaration)
        stable = false;
      ts.forEachChild(node, writesTarget);
    };
    const scan = (node: ts.Node) => {
      if (!stable) return;
      if (--budget < 0) {
        stable = false;
        return;
      }
      if (
        ts.isBinaryExpression(node) &&
        node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
        node.operatorToken.kind <= ts.SyntaxKind.LastAssignment
      )
        writesTarget(node.left);
      if (
        (ts.isPrefixUnaryExpression(node) ||
          ts.isPostfixUnaryExpression(node)) &&
        [ts.SyntaxKind.PlusPlusToken, ts.SyntaxKind.MinusMinusToken].includes(
          node.operator,
        )
      )
        writesTarget(node.operand);
      if (
        ts.isCallExpression(node) &&
        ts.isIdentifier(node.expression) &&
        node.expression.text === "eval"
      )
        stable = false;
      ts.forEachChild(node, scan);
    };
    scan(sf);
    targetChecks.set(declaration, stable);
    return stable;
  }

  function evaluate(raw: ts.Expression, action: string, depth: number): Value {
    if (--budget < 0 || depth > 16) return fail(raw, "source-model-budget");
    const e = peel(raw);
    if (ts.isStringLiteralLike(e)) return e.text;
    if (ts.isNumericLiteral(e)) return Number(e.text);
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true;
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false;
    if (e.kind === ts.SyntaxKind.NullKeyword) return null;
    if (ts.isIdentifier(e)) {
      const d = model.declaration(e);
      if (d && locals.has(d)) return locals.get(d);
      return fail(e, "unresolved-value-binding");
    }
    if (ts.isPropertyAccessExpression(e)) {
      const base = evaluate(e.expression, action, depth + 1);
      if (!primitive(base)) {
        if (base.kind === "mock" && e.name.text === "mock")
          return { kind: "context", mock: base };
        if (base.kind === "context" && e.name.text === "calls")
          return snapshot(base.mock, e, "history");
        if (base.kind === "history" && e.name.text === "length")
          return { ...base, kind: "count" };
      }
      return fail(e, "unsupported-property-read");
    }
    if (!ts.isCallExpression(e)) return fail(e, "unsupported-expression");
    const callee = peel(e.expression);
    if (model.nativeMock(e)) {
      const [receiver, method, replacement] = e.arguments;
      if (
        !ts.isPropertyAccessExpression(callee) ||
        callee.name.text !== "method" ||
        e.arguments.length !== 3 ||
        !model.globalConsole(receiver) ||
        !ts.isStringLiteralLike(method) ||
        !["log", "error"].includes(method.text) ||
        !ts.isArrowFunction(replacement) ||
        replacement.parameters.length ||
        replacement.modifiers?.length ||
        !ts.isBlock(replacement.body) ||
        replacement.body.statements.length
      )
        return fail(e, "unsupported-mock-installation");
      const target = `console.${method.text}`;
      const mock: Mock = {
        kind: "mock",
        target,
        source: model.location(e),
        previous: installed.get(target),
        calls: [],
      };
      installed.set(target, mock);
      return mock;
    }
    if (
      ts.isPropertyAccessExpression(callee) &&
      model.globalConsole(callee.expression) &&
      ["log", "error"].includes(callee.name.text)
    ) {
      for (const arg of e.arguments)
        if (!primitive(evaluate(arg, action, depth + 1)))
          return fail(arg, "nonprimitive-console-argument");
      record(installed.get(`console.${callee.name.text}`), e, action);
      return undefined;
    }
    if (
      ts.isPropertyAccessExpression(callee) &&
      ["callCount", "resetCalls", "restore"].includes(callee.name.text)
    ) {
      const base = evaluate(callee.expression, action, depth + 1);
      if (primitive(base) || base.kind !== "context" || e.arguments.length)
        return fail(e, "unsupported-mock-operation");
      if (callee.name.text === "callCount")
        return snapshot(base.mock, e, "count");
      if (callee.name.text === "resetCalls") {
        base.mock.calls = [];
        base.mock.resetAt = model.location(e);
      } else installed.set(base.mock.target, base.mock.previous);
      return undefined;
    }
    const predicate = model.nativePredicate(e);
    if (predicate) {
      if (e.arguments.length < 2 || e.arguments.length > 3)
        return fail(e, "unsupported-comparison-arity");
      const values = e.arguments.map((arg) => evaluate(arg, action, depth + 1));
      if (values.length === 3 && !primitive(values[2]))
        return fail(e, "unsupported-assertion-message");
      const [a, b] = values;
      const actual =
        !primitive(a) && a.kind === "count"
          ? a
          : !primitive(b) && b.kind === "count"
            ? b
            : undefined;
      const expected = actual === a ? b : a;
      if (actual) {
        if (
          typeof expected !== "number" ||
          !Number.isSafeInteger(expected) ||
          expected < 0
        )
          return fail(e, "count-expectation-not-independent-integer");
        if (actual.evidence.observedCount !== expected)
          return fail(e, "source-count-disagrees-with-passing-assertion");
        checks.set(e, { ...actual.evidence, expectedCount: expected });
      } else if (!primitive(a) || !primitive(b))
        return fail(e, "unsupported-comparison-operand");
      return undefined;
    }
    const declaration = model.declaration(callee);
    const saved = declaration && locals.get(declaration);
    if (saved && !primitive(saved) && saved.kind === "mock") {
      for (const arg of e.arguments)
        if (!primitive(evaluate(arg, action, depth + 1)))
          return fail(arg, "nonprimitive-mock-argument");
      record(saved, e, action);
      return undefined;
    }
    const target =
      declaration && ts.isVariableDeclaration(declaration)
        ? declaration.initializer
        : declaration;
    if (
      declaration &&
      ts.isVariableDeclaration(declaration) &&
      !(declaration.parent.flags & ts.NodeFlags.Const)
    )
      return fail(e, "mutable-call-target");
    if (
      !target ||
      !(
        ts.isFunctionDeclaration(target) ||
        ts.isArrowFunction(target) ||
        ts.isFunctionExpression(target)
      ) ||
      !target.body ||
      !model.production(target) ||
      target.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword) ||
      ("asteriskToken" in target && target.asteriskToken)
    )
      return fail(e, "unsupported-call-target");
    if (!declaration || !stableTarget(declaration))
      return fail(e, "mutable-target-or-unsupported-module-initialization");
    if (
      target.parameters.some(
        (p) => !ts.isIdentifier(p.name) || p.dotDotDotToken || p.initializer,
      ) ||
      e.arguments.length !== target.parameters.length
    )
      return fail(e, "unsupported-call-parameters");
    const args = e.arguments.map((arg) => evaluate(arg, action, depth + 1));
    if (args.some((arg) => !primitive(arg)))
      return fail(e, "escaping-nonprimitive-argument");
    const previous = new Map(locals);
    target.parameters.forEach((parameter, index) =>
      locals.set(parameter, args[index]),
    );
    try {
      if (!ts.isBlock(target.body))
        return evaluate(target.body, action, depth + 1);
      for (const statement of target.body.statements) {
        if (ts.isReturnStatement(statement))
          return statement.expression
            ? evaluate(statement.expression, action, depth + 1)
            : undefined;
        execute(statement, action, depth + 1);
      }
      return undefined;
    } finally {
      locals.clear();
      for (const [key, value] of previous) locals.set(key, value);
    }
  }

  function execute(statement: ts.Statement, action: string, depth: number) {
    if (--budget < 0) return fail(statement, "source-model-budget");
    if (
      ts.isVariableStatement(statement) &&
      statement.declarationList.flags & ts.NodeFlags.Const
    ) {
      for (const d of statement.declarationList.declarations) {
        if (!ts.isIdentifier(d.name) || !d.initializer)
          return fail(d, "unsupported-local-declaration");
        locals.set(d, evaluate(d.initializer, action, depth + 1));
      }
    } else if (ts.isExpressionStatement(statement))
      evaluate(statement.expression, action, depth + 1);
    else if (!ts.isEmptyStatement(statement))
      fail(statement, "unsupported-statement");
  }

  let limitation: string | undefined;
  try {
    if (
      !(ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) ||
      !ts.isBlock(fn.body) ||
      fn.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword)
    )
      fail(fn, "unsupported-test-body");
    // No branching, loops, async suspension, escaped mocks or opaque calls are
    // accepted before a checked assertion. A later unsupported statement does
    // not invalidate an earlier captured count.
    if (
      (ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) &&
      ts.isBlock(fn.body)
    )
      for (const statement of fn.body.statements)
        execute(statement, model.location(statement), 0);
  } catch (error) {
    if (!(error instanceof Unsupported)) throw error;
    limitation = error.message;
  }
  return { checks, limitation };
}
