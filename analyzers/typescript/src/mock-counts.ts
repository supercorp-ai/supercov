/** A bounded synchronous source model, not general execution or value-flow analysis. */
import type ts from "typescript";
import type { SyntaxAPI } from "./frontend.js";

export interface MockCountEvidence {
  model: "node-sync-console-count-v2";
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
  /** Selection of copied history; an empty selection is not an empty mock. */
  historySelections?: {
    source: string;
    inputCount: number;
    from: number;
    to: number;
  }[];
  rowBinding?: MockCountRowBinding;
}

type RowValue = string | number | boolean | null;
export interface MockCountRowBinding {
  model: "node-test-for-of-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  loop?: string;
  table?: string;
  row?: string;
  rowIndex?: number;
  title?: string;
  bindings?: { declaration: string; name: string; value: RowValue }[];
}
interface BoundRow {
  bindings: Map<ts.Declaration, RowValue>;
  evidence: MockCountRowBinding;
}

/** A closed registration table, not title-prefix guessing or runtime-value inference. */
export function sourceTestRows(
  syntax: SyntaxAPI,
  fn: ts.Node,
  model: Pick<Model, "declaration" | "location"> & {
    nativeTest(call: ts.CallExpression): boolean;
  },
): { rows: BoundRow[]; reason?: string } | undefined {
  const ts = syntax;
  let ancestor: ts.Node | undefined = fn.parent;
  while (
    ancestor &&
    !ts.isForOfStatement(ancestor) &&
    !ts.isSourceFile(ancestor)
  )
    ancestor = ancestor.parent;
  if (!ancestor || !ts.isForOfStatement(ancestor)) return undefined;
  const unsupported = (reason: string) => ({ rows: [], reason });
  const loop = ancestor,
    sf = fn.getSourceFile();
  const registration = fn.parent;
  if (
    !ts.isSourceFile(loop.parent) ||
    loop.awaitModifier ||
    !ts.isBlock(loop.statement) ||
    loop.statement.statements.length !== 1 ||
    !ts.isCallExpression(registration) ||
    registration.arguments.length !== 2 ||
    registration.arguments[1] !== fn ||
    !model.nativeTest(registration) ||
    !ts.isExpressionStatement(registration.parent) ||
    registration.parent !== loop.statement.statements[0]
  )
    return unsupported("unsupported-row-registration");
  const peel = (raw: ts.Expression): ts.Expression => {
    let e = raw;
    while (
      ts.isParenthesizedExpression(e) ||
      ts.isAsExpression(e) ||
      ts.isTypeAssertionExpression(e) ||
      ts.isNonNullExpression(e) ||
      ts.isSatisfiesExpression(e)
    )
      e = e.expression;
    return e;
  };
  const iterable = peel(loop.expression);
  if (!ts.isIdentifier(iterable)) return unsupported("unsupported-row-table");
  const tableDeclaration = model.declaration(iterable);
  if (
    !tableDeclaration ||
    !ts.isVariableDeclaration(tableDeclaration) ||
    !ts.isIdentifier(tableDeclaration.name) ||
    !tableDeclaration.initializer ||
    !ts.isVariableDeclarationList(tableDeclaration.parent) ||
    !(tableDeclaration.parent.flags & ts.NodeFlags.Const) ||
    !ts.isVariableStatement(tableDeclaration.parent.parent) ||
    tableDeclaration.parent.parent.parent !== sf ||
    tableDeclaration.end >= loop.getStart(sf) ||
    tableDeclaration.parent.parent.modifiers?.some(
      (m) => m.kind === ts.SyntaxKind.ExportKeyword,
    )
  )
    return unsupported("unsupported-row-table-binding");
  const table = peel(tableDeclaration.initializer);
  if (
    !ts.isArrayLiteralExpression(table) ||
    !table.elements.length ||
    table.elements.length > 256
  )
    return unsupported("unsupported-row-table");
  // The literal's sole read must be this loop. Alias creation, mutation, another
  // consumer or direct eval makes a private const array insufficient evidence.
  let exclusive = true,
    budget = 16384;
  const scan = (node: ts.Node) => {
    if (!exclusive) return;
    if (--budget < 0) {
      exclusive = false;
      return;
    }
    if (
      ts.isIdentifier(node) &&
      model.declaration(node) === tableDeclaration &&
      node !== tableDeclaration.name &&
      node !== iterable
    )
      exclusive = false;
    if (ts.isIdentifier(node) && node.text === "eval") exclusive = false;
    ts.forEachChild(node, scan);
  };
  scan(sf);
  if (!exclusive) return unsupported("row-table-escapes-or-may-change");
  if (
    !ts.isVariableDeclarationList(loop.initializer) ||
    !(loop.initializer.flags & ts.NodeFlags.Const) ||
    loop.initializer.declarations.length !== 1
  )
    return unsupported("mutable-or-unsupported-row-bindings");
  const binding = loop.initializer.declarations[0];
  if (
    !ts.isObjectBindingPattern(binding.name) ||
    binding.initializer ||
    binding.name.elements.some(
      (e) =>
        !ts.isIdentifier(e.name) ||
        e.dotDotDotToken ||
        e.initializer ||
        (e.propertyName &&
          !(
            ts.isIdentifier(e.propertyName) ||
            ts.isStringLiteralLike(e.propertyName)
          )),
    )
  )
    return unsupported("unsupported-row-destructuring");
  const literal = (raw: ts.Expression): RowValue | undefined => {
    const e = peel(raw);
    if (ts.isStringLiteralLike(e)) return e.text;
    if (ts.isNumericLiteral(e) && Number.isFinite(Number(e.text)))
      return Number(e.text);
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true;
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false;
    if (e.kind === ts.SyntaxKind.NullKeyword) return null;
    return undefined;
  };
  const rows: BoundRow[] = [],
    titles = new Set<string>();
  for (const [rowIndex, raw] of table.elements.entries()) {
    const row = peel(raw);
    if (!ts.isObjectLiteralExpression(row))
      return unsupported("unsupported-row-value");
    const properties = new Map<string, RowValue>();
    for (const member of row.properties) {
      if (
        !ts.isPropertyAssignment(member) ||
        !(ts.isIdentifier(member.name) || ts.isStringLiteralLike(member.name))
      )
        return unsupported("unsupported-row-value");
      const value = literal(member.initializer),
        key = member.name.text;
      if (value === undefined || key === "__proto__" || properties.has(key))
        return unsupported("unsupported-row-value");
      properties.set(key, value);
    }
    const bindings = new Map<ts.Declaration, RowValue>();
    const evidenceBindings: NonNullable<MockCountRowBinding["bindings"]> = [];
    for (const element of binding.name.elements) {
      const key = (element.propertyName ?? element.name) as
        | ts.Identifier
        | ts.StringLiteralLike;
      if (!properties.has(key.text))
        return unsupported("missing-row-own-property");
      const value = properties.get(key.text)!;
      bindings.set(element, value);
      evidenceBindings.push({
        declaration: model.location(element),
        name: (element.name as ts.Identifier).text,
        value,
      });
    }
    const name = peel(registration.arguments[0]);
    let title: string;
    if (ts.isStringLiteralLike(name)) title = name.text;
    else if (ts.isTemplateExpression(name)) {
      title = name.head.text;
      for (const span of name.templateSpans) {
        const expr = peel(span.expression),
          declaration = model.declaration(expr);
        if (
          !ts.isIdentifier(expr) ||
          !declaration ||
          !bindings.has(declaration)
        )
          return unsupported("unsupported-row-title");
        title += String(bindings.get(declaration)) + span.literal.text;
      }
    } else return unsupported("unsupported-row-title");
    if (titles.has(title)) return unsupported("ambiguous-row-title");
    titles.add(title);
    rows.push({
      bindings,
      evidence: {
        model: "node-test-for-of-v1",
        status: "source-checked",
        loop: model.location(loop),
        table: model.location(table),
        row: model.location(row),
        rowIndex,
        title,
        bindings: evidenceBindings,
      },
    });
  }
  return { rows };
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

export interface CallOmissionEvidence {
  model: "node-first-test-call-omission-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  outcome?: "rejected" | "not-rejected";
  scope?: "first-synchronous-test";
  assertionSource?: string;
  callSource?: string;
  callbackSource?: string;
  instance?: string;
  expectedCount?: number;
  originalCount?: number;
  omittedCount?: number;
}

interface OmissionTrial {
  assertion: ts.CallExpression;
  module: ts.SourceFile;
  omit?: ts.CallExpression;
}

export function analyzeMockCounts(
  syntax: SyntaxAPI,
  fn: ts.Node,
  model: Model,
  row?: BoundRow,
) {
  return runMockCounts(syntax, fn, model, row);
}

function runMockCounts(
  syntax: SyntaxAPI,
  fn: ts.Node,
  model: Model,
  row?: BoundRow,
  trial?: OmissionTrial,
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
  type Snapshot =
    | { kind: "count"; evidence: MockCountEvidence }
    | { kind: "history"; evidence: MockCountEvidence };
  type FunctionNode =
    | ts.FunctionDeclaration
    | ts.ArrowFunction
    | ts.FunctionExpression;
  type Closure = {
    kind: "closure";
    node: FunctionNode;
    environment: Map<ts.Declaration, Value>;
  };
  type Aggregate = {
    kind: "object" | "array";
    properties: Map<string, Value>;
    moduleOwned: boolean;
  };
  type Value =
    | Primitive
    | Mock
    | Snapshot
    | Closure
    | Aggregate
    | { kind: "context"; mock: Mock };
  const checks = new Map<ts.CallExpression, MockCountEvidence>();
  const installed = new Map<string, Mock | undefined>();
  let locals = new Map<ts.Declaration, Value>(row?.bindings);
  const moduleChecks = new Map<ts.SourceFile, boolean>();
  const modules = new Map<ts.SourceFile, Map<ts.Declaration, Value>>();
  const loading = new Set<ts.SourceFile>();
  let initializing = false;
  let budget = 4096;
  class Unsupported extends Error {}
  class ReachedAssertion extends Error {}
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
      model: "node-sync-console-count-v2",
      status: "source-checked",
      instance: mock.source,
      createdAt: mock.source,
      resetAt: mock.resetAt,
      readAt: model.location(node),
      installedAtRead: installed.get(mock.target) === mock,
      observedCount: mock.calls.length,
      calls: mock.calls.map((call) => ({ ...call })),
      ...(row ? { rowBinding: row.evidence } : {}),
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
    const sf = declaration.getSourceFile();
    let safeModule = moduleChecks.get(sf);
    if (safeModule !== undefined) return safeModule;
    if (safeModule === undefined) {
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
              (d) => ts.isIdentifier(d.name) && !!d.initializer,
            )),
      );
    }
    if (!safeModule) {
      moduleChecks.set(sf, false);
      return false;
    }
    let stable = true;
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
        stable = false;
      if (
        (ts.isPrefixUnaryExpression(node) ||
          ts.isPostfixUnaryExpression(node)) &&
        [ts.SyntaxKind.PlusPlusToken, ts.SyntaxKind.MinusMinusToken].includes(
          node.operator,
        )
      )
        stable = false;
      if (ts.isDeleteExpression(node)) stable = false;
      if (ts.isIdentifier(node) && node.text === "eval") stable = false;
      ts.forEachChild(node, scan);
    };
    scan(sf);
    moduleChecks.set(sf, stable);
    return stable;
  }

  // Initialization is evaluated with effects forbidden, never replayed as work
  // performed under a test's mock. Objects allocated here stay shared/unknown:
  // const prevents rebinding, not mutation by another caller or earlier test.
  function moduleEnvironment(declaration: ts.Declaration, depth: number) {
    const sf = declaration.getSourceFile();
    if (trial && sf !== trial.module)
      return fail(declaration, "omission-module-outside-checked-scope");
    if (loading.has(sf))
      return fail(declaration, "cyclic-or-forward-module-binding");
    const cached = modules.get(sf);
    if (cached) return cached;
    if (!stableTarget(declaration))
      return fail(
        declaration,
        "mutable-target-or-unsupported-module-initialization",
      );
    const previous = locals,
      wasInitializing = initializing;
    locals = new Map();
    initializing = true;
    loading.add(sf);
    modules.set(sf, locals);
    try {
      for (const statement of sf.statements)
        if (ts.isFunctionDeclaration(statement))
          locals.set(statement, {
            kind: "closure",
            node: statement,
            environment: locals,
          });
      for (const statement of sf.statements)
        if (ts.isVariableStatement(statement))
          execute(statement, model.location(statement), depth + 1);
      return locals;
    } finally {
      locals = previous;
      initializing = wasInitializing;
      loading.delete(sf);
    }
  }

  function accessible(value: Value, node: ts.Node): Value {
    if (
      !initializing &&
      !primitive(value) &&
      (value.kind === "object" || value.kind === "array") &&
      value.moduleOwned &&
      !trial
    )
      return fail(node, "shared-module-object-history");
    return value;
  }

  function sourceValue(value: Value): boolean {
    if (--budget < 0) return fail(fn, "source-model-budget");
    return (
      primitive(value) ||
      value.kind === "closure" ||
      ((value.kind === "object" || value.kind === "array") &&
        [...value.properties.values()].every(sourceValue))
    );
  }

  function propertyName(node: ts.PropertyName): string {
    if (
      ts.isIdentifier(node) ||
      ts.isStringLiteralLike(node) ||
      ts.isNumericLiteral(node)
    )
      return node.text;
    return fail(node, "computed-property-name");
  }

  function bind(
    name: ts.BindingName,
    declaration: ts.Declaration,
    value: Value,
    action: string,
    depth: number,
  ): void {
    if (ts.isIdentifier(name)) {
      locals.set(declaration, value);
      return;
    }
    if (
      !ts.isObjectBindingPattern(name) ||
      primitive(value) ||
      value.kind !== "object"
    )
      return fail(name, "unsupported-parameter-binding");
    accessible(value, name);
    for (const element of name.elements) {
      if (element.dotDotDotToken || !ts.isIdentifier(element.name))
        return fail(element, "unsupported-parameter-binding");
      const key = propertyName(element.propertyName ?? element.name);
      // Missing properties could be inherited. No prototype lookup is guessed.
      if (!value.properties.has(key))
        return fail(element, "missing-own-property");
      let item = value.properties.get(key);
      if (item === undefined && element.initializer)
        item = evaluate(element.initializer, action, depth + 1);
      bind(element.name, element, item, action, depth + 1);
    }
  }

  function invoke(
    closure: Closure,
    args: Value[],
    at: ts.Node,
    action: string,
    depth: number,
  ): Value {
    const target = closure.node;
    if (
      !target.body ||
      !model.production(target) ||
      target.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword) ||
      ("asteriskToken" in target && target.asteriskToken)
    )
      return fail(at, "unsupported-call-target");
    if (!stableTarget(target))
      return fail(at, "mutable-target-or-unsupported-module-initialization");
    if (args.some((arg) => !sourceValue(arg)))
      return fail(at, "escaping-nonprimitive-argument");
    const previous = locals;
    locals = new Map(closure.environment);
    try {
      target.parameters.forEach((parameter, index) => {
        let value: Value = args[index];
        if (parameter.dotDotDotToken) {
          value = {
            kind: "array",
            moduleOwned: initializing,
            properties: new Map(
              args.slice(index).map((v, i) => [String(i), v]),
            ),
          };
        } else if (value === undefined && parameter.initializer)
          value = evaluate(parameter.initializer, action, depth + 1);
        bind(parameter.name, parameter, value, action, depth + 1);
      });
      if (!ts.isBlock(target.body))
        return evaluate(target.body, action, depth + 1);
      for (const statement of target.body.statements) {
        const returned = execute(statement, action, depth + 1);
        if (returned) return returned.value;
      }
      return undefined;
    } finally {
      locals = previous;
    }
  }

  function argumentsOf(
    call: ts.CallExpression,
    action: string,
    depth: number,
  ): Value[] {
    const result: Value[] = [];
    for (const arg of call.arguments) {
      if (ts.isSpreadElement(arg)) {
        const value = evaluate(arg.expression, action, depth + 1);
        if (primitive(value) || value.kind !== "array")
          return fail(arg, "unsupported-spread");
        accessible(value, arg);
        result.push(...value.properties.values());
      } else result.push(evaluate(arg, action, depth + 1));
      if (result.length > 4096) return fail(call, "source-model-budget");
    }
    return result;
  }

  function evaluate(raw: ts.Expression, action: string, depth: number): Value {
    if (--budget < 0 || depth > 32) return fail(raw, "source-model-budget");
    const e = peel(raw);
    if (ts.isStringLiteralLike(e)) return e.text;
    if (ts.isNumericLiteral(e)) return Number(e.text);
    if (
      ts.isPrefixUnaryExpression(e) &&
      e.operator === ts.SyntaxKind.MinusToken
    ) {
      const operand = evaluate(e.operand, action, depth + 1);
      if (typeof operand !== "number" || !Number.isFinite(operand))
        return fail(e, "unsupported-unary-operand");
      return -operand;
    }
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true;
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false;
    if (e.kind === ts.SyntaxKind.NullKeyword) return null;
    if (ts.isIdentifier(e)) {
      const d = model.declaration(e);
      if (d && locals.has(d)) return accessible(locals.get(d), e);
      if (d && model.production(d)) {
        const environment = moduleEnvironment(d, depth);
        if (environment.has(d)) return accessible(environment.get(d), e);
      }
      return fail(e, "unresolved-value-binding");
    }
    if (ts.isArrowFunction(e) || ts.isFunctionExpression(e))
      return { kind: "closure", node: e, environment: locals };
    if (ts.isObjectLiteralExpression(e)) {
      const properties = new Map<string, Value>();
      for (const item of e.properties) {
        if (
          !(
            ts.isPropertyAssignment(item) ||
            ts.isShorthandPropertyAssignment(item)
          )
        )
          return fail(item, "unsupported-object-member");
        const key = propertyName(item.name);
        if (key === "__proto__")
          return fail(item, "unsupported-object-prototype");
        properties.set(
          key,
          evaluate(
            ts.isPropertyAssignment(item) ? item.initializer : item.name,
            action,
            depth + 1,
          ),
        );
      }
      return { kind: "object", properties, moduleOwned: initializing };
    }
    if (ts.isArrayLiteralExpression(e)) {
      const properties = new Map<string, Value>();
      for (const item of e.elements) {
        if (ts.isOmittedExpression(item) || ts.isSpreadElement(item))
          return fail(item, "unsupported-array-member");
        properties.set(
          String(properties.size),
          evaluate(item, action, depth + 1),
        );
      }
      return { kind: "array", properties, moduleOwned: initializing };
    }
    if (ts.isConditionalExpression(e)) {
      const condition = evaluate(e.condition, action, depth + 1);
      if (!primitive(condition)) return fail(e, "nonprimitive-condition");
      return evaluate(condition ? e.whenTrue : e.whenFalse, action, depth + 1);
    }
    if (
      ts.isBinaryExpression(e) &&
      [
        ts.SyntaxKind.EqualsEqualsEqualsToken,
        ts.SyntaxKind.ExclamationEqualsEqualsToken,
      ].includes(e.operatorToken.kind)
    ) {
      const left = evaluate(e.left, action, depth + 1),
        right = evaluate(e.right, action, depth + 1);
      if (!primitive(left) || !primitive(right))
        return fail(e, "nonprimitive-comparison");
      return e.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken
        ? left === right
        : left !== right;
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
        if (base.kind === "object" || base.kind === "array") {
          accessible(base, e);
          if (base.kind === "array" && e.name.text === "length")
            return base.properties.size;
          if (base.properties.has(e.name.text))
            return accessible(base.properties.get(e.name.text), e);
        }
      }
      return fail(e, "unsupported-property-read");
    }
    if (!ts.isCallExpression(e)) return fail(e, "unsupported-expression");
    // The checked edit replaces this expression-bodied arrow with () => undefined.
    // Its original arguments inside the body are NOT evaluated by that edit.
    // No source is executed and no test code is modified during this query.
    if (trial?.omit === e) return undefined;
    const callee = peel(e.expression);
    if (
      ts.isPropertyAccessExpression(callee) &&
      ["map", "slice"].includes(callee.name.text)
    ) {
      // Resolve the receiver once, before evaluating arguments. A factory call
      // here can itself log; history getters also snapshot before index effects.
      const base = evaluate(callee.expression, action, depth + 1);
      if (primitive(base)) return fail(callee, "unsupported-array-receiver");
      if (base.kind === "object") {
        accessible(base, callee);
        const target = base.properties.get(callee.name.text);
        if (!target || primitive(target) || target.kind !== "closure")
          return fail(callee, "unsupported-array-receiver");
        // A source method named map/slice is not the native array operation.
        return accessible(
          invoke(
            target,
            argumentsOf(e, action, depth + 1),
            e,
            action,
            depth + 1,
          ),
          e,
        );
      }
      if (base.kind !== "array" && base.kind !== "history")
        return fail(callee, "unsupported-array-receiver");
      if (base.kind === "array") accessible(base, callee);
      const args = argumentsOf(e, action, depth + 1);
      if (callee.name.text === "map") {
        if (
          base.kind !== "array" ||
          args.length !== 1 ||
          primitive(args[0]) ||
          args[0].kind !== "closure"
        )
          return fail(e, "unsupported-array-map-callback");
        const properties = new Map<string, Value>();
        // All admitted arrays are fresh, dense arrays with native prototypes.
        // Mutation/escape in a callback remains unsupported by invoke/evaluate.
        const length = base.properties.size;
        for (let index = 0; index < length; index++)
          properties.set(
            String(index),
            invoke(
              args[0],
              [base.properties.get(String(index)), index, base],
              e,
              action,
              depth + 1,
            ),
          );
        return { kind: "array", properties, moduleOwned: initializing };
      }
      if (
        args.length > 2 ||
        args.some(
          (value) =>
            value !== undefined &&
            (typeof value !== "number" || !Number.isSafeInteger(value)),
        )
      )
        return fail(e, "unsupported-slice-index");
      const length =
        base.kind === "history"
          ? base.evidence.calls!.length
          : base.properties.size;
      const clamp = (n: number) =>
        n < 0 ? Math.max(length + n, 0) : Math.min(n, length);
      const from = clamp((args[0] as number | undefined) ?? 0);
      const to = Math.max(
        from,
        clamp((args[1] as number | undefined) ?? length),
      );
      if (base.kind === "history") {
        const calls = base.evidence
          .calls!.slice(from, to)
          .map((call) => ({ ...call }));
        return {
          kind: "history",
          evidence: {
            ...base.evidence,
            calls,
            observedCount: calls.length,
            historySelections: [
              ...(base.evidence.historySelections ?? []),
              { source: model.location(e), inputCount: length, from, to },
            ],
          },
        };
      }
      return {
        kind: "array",
        moduleOwned: initializing,
        properties: new Map(
          [...base.properties.values()]
            .slice(from, to)
            .map((value, index) => [String(index), value]),
        ),
      };
    }
    if (model.nativeMock(e)) {
      if (initializing) return fail(e, "effectful-module-initialization");
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
      if (initializing) return fail(e, "effectful-module-initialization");
      // JS resolves the callee before argument evaluation (which may itself call it).
      const mock = installed.get(`console.${callee.name.text}`);
      const args = argumentsOf(e, action, depth + 1);
      // An accepted installed mock has an empty replacement: Node records the
      // argument references but does not format/inspect them. This establishes
      // a call count only, not argument identity or value protection. Without
      // that mock, native console formatting may execute user code.
      if (args.some((arg) => (mock ? !sourceValue(arg) : !primitive(arg))))
        return fail(e, "nonprimitive-console-argument");
      record(mock, e, action);
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
      if (initializing) return fail(e, "effectful-module-initialization");
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
        if (
          actual.evidence.observedCount !== expected &&
          trial?.assertion !== e
        )
          return fail(e, "source-count-disagrees-with-passing-assertion");
        checks.set(e, { ...actual.evidence, expectedCount: expected });
      } else if (!primitive(a) || !primitive(b))
        return fail(e, "unsupported-comparison-operand");
      if (trial?.assertion === e) throw new ReachedAssertion();
      return undefined;
    }
    const declaration = model.declaration(callee);
    const saved = declaration && locals.get(declaration);
    if (saved && !primitive(saved) && saved.kind === "mock") {
      for (const arg of e.arguments)
        if (!sourceValue(evaluate(arg, action, depth + 1)))
          return fail(arg, "nonprimitive-mock-argument");
      record(saved, e, action);
      return undefined;
    }
    if (
      declaration &&
      ts.isVariableDeclaration(declaration) &&
      !(declaration.parent.flags & ts.NodeFlags.Const)
    )
      return fail(e, "mutable-call-target");
    let target: Value;
    // Namespace import access has a resolved source declaration rather than a
    // modeled namespace object. Do not bypass a runtime receiver with a mere name.
    if (
      declaration &&
      model.production(declaration) &&
      (ts.isFunctionDeclaration(declaration) ||
        ts.isVariableDeclaration(declaration)) &&
      !locals.has(declaration)
    )
      target = moduleEnvironment(declaration, depth).get(declaration);
    else target = evaluate(callee, action, depth + 1);
    if (primitive(target) || target.kind !== "closure")
      return fail(e, "unsupported-call-target");
    return accessible(
      invoke(target, argumentsOf(e, action, depth + 1), e, action, depth + 1),
      e,
    );
  }

  function execute(
    statement: ts.Statement,
    action: string,
    depth: number,
  ): { value: Value } | undefined {
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
    else if (ts.isReturnStatement(statement) && model.production(statement))
      return {
        value: statement.expression
          ? evaluate(statement.expression, action, depth + 1)
          : undefined,
      };
    else if (ts.isIfStatement(statement) && model.production(statement)) {
      const condition = evaluate(statement.expression, action, depth + 1);
      if (!primitive(condition))
        return fail(statement, "nonprimitive-condition");
      const branch = condition
        ? statement.thenStatement
        : statement.elseStatement;
      if (branch) return execute(branch, action, depth + 1);
    } else if (ts.isBlock(statement) && model.production(statement)) {
      for (const child of statement.statements) {
        const returned = execute(child, action, depth + 1);
        if (returned) return returned;
      }
    } else if (!ts.isEmptyStatement(statement))
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
    // No test branching, loops, async suspension, escaped mocks or opaque calls are
    // accepted before a checked assertion. A later unsupported statement does
    // not invalidate an earlier captured count.
    if (
      (ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) &&
      ts.isBlock(fn.body)
    )
      for (const statement of fn.body.statements)
        execute(statement, model.location(statement), 0);
  } catch (error) {
    if (error instanceof Unsupported) limitation = error.message;
    else if (!(error instanceof ReachedAssertion)) throw error;
  }
  return { checks, limitation };
}

/** A hint selects this small proof recipe, never permission to assume shared state safe.
 * The first synchronous callback has no earlier caller of its imported module in
 * this closed test file. Normal isolated Node test loading/scheduling and unmodified
 * built-ins are model assumptions, not facts proved by the source inspection.
 */
export function analyzeFirstTestOmission(
  syntax: SyntaxAPI,
  fn: ts.Node,
  assertion: ts.CallExpression,
  call: ts.CallExpression,
  model: Model & { nativeTest(call: ts.CallExpression): boolean },
  row?: BoundRow,
): CallOmissionEvidence {
  const ts = syntax;
  const base: CallOmissionEvidence = {
    model: "node-first-test-call-omission-v1",
    status: "unresolved",
  };
  const limit = (reason: string) => ({ ...base, reason });
  const sf = fn.getSourceFile(),
    production = call.getSourceFile();
  const parent = call.parent;
  if (
    !ts.isArrowFunction(parent) ||
    parent.body !== call ||
    parent.modifiers?.length ||
    parent.parameters.some((p) => !ts.isIdentifier(p.name) || p.initializer) ||
    !ts.isPropertyAccessExpression(call.expression) ||
    !model.globalConsole(call.expression.expression) ||
    !["log", "error"].includes(call.expression.name.text) ||
    !model.production(call)
  )
    return limit("omission-target-not-direct-console-callback");
  if (
    !ts.isArrowFunction(fn) ||
    !ts.isBlock(fn.body) ||
    fn.modifiers?.length ||
    fn.parameters.length !== 1 ||
    !ts.isIdentifier(fn.parameters[0].name) ||
    fn.parameters[0].initializer ||
    fn.parameters[0].dotDotDotToken ||
    !ts.isExpressionStatement(assertion.parent) ||
    assertion.parent.parent !== fn.body ||
    !ts.isCallExpression(fn.parent) ||
    !model.nativeTest(fn.parent) ||
    fn.parent.arguments.length !== 2 ||
    fn.parent.arguments[1] !== fn ||
    model.nativePredicate(assertion) !== "node-same-value"
  )
    return limit("omission-requires-direct-synchronous-count-assertion");
  const registration = fn.parent;
  let registrationStatement: ts.Node = registration.parent;
  while (registrationStatement.parent && registrationStatement.parent !== sf)
    registrationStatement = registrationStatement.parent;
  if (row && row.evidence.rowIndex !== 0)
    return limit("omission-not-first-source-row");
  if (
    !row &&
    (!ts.isExpressionStatement(registrationStatement) ||
      registrationStatement.expression !== registration ||
      !ts.isStringLiteralLike(registration.arguments[0]))
  )
    return limit("omission-unsupported-registration");

  // Only native imports and the selected production module may initialize before
  // registration. A type-only import is erased; an unrelated import is not inert.
  const erasedImport = (s: ts.ImportDeclaration) => {
    const clause = s.importClause;
    if (!clause) return false;
    if (clause.isTypeOnly) return true;
    const bindings = clause.namedBindings;
    return (
      !clause.name &&
      !!bindings &&
      ts.isNamedImports(bindings) &&
      bindings.elements.length > 0 &&
      bindings.elements.every((e) => {
        const d = model.declaration(e.name);
        return (
          e.isTypeOnly ||
          (!!d &&
            (ts.isInterfaceDeclaration(d) || ts.isTypeAliasDeclaration(d)))
        );
      })
    );
  };
  const nativeImports = new Set([
    "node:test",
    "node:assert/strict",
    "node:assert",
    "assert/strict",
    "assert",
  ]);
  let found = false;
  for (const s of sf.statements) {
    if (ts.isImportDeclaration(s)) {
      if (erasedImport(s)) continue;
      if (!ts.isStringLiteralLike(s.moduleSpecifier) || !s.importClause)
        return limit("omission-test-import-outside-scope");
      if (nativeImports.has(s.moduleSpecifier.text)) continue;
      const c = s.importClause;
      const names = [
        c.name,
        ...(c.namedBindings && ts.isNamedImports(c.namedBindings)
          ? c.namedBindings.elements.map((e) => e.name)
          : []),
      ].filter((n) => n !== undefined);
      if (
        !names.length ||
        (c.namedBindings && ts.isNamespaceImport(c.namedBindings)) ||
        names.some((n) => model.declaration(n)?.getSourceFile() !== production)
      )
        return limit("omission-test-import-outside-scope");
      continue;
    }
    if (
      ts.isEmptyStatement(s) ||
      ts.isInterfaceDeclaration(s) ||
      ts.isTypeAliasDeclaration(s)
    )
      continue;
    if (
      row &&
      ts.isVariableStatement(s) &&
      ts.isForOfStatement(registrationStatement)
    ) {
      const iterable = registrationStatement.expression;
      if (
        s.declarationList.declarations.length === 1 &&
        model.declaration(iterable) === s.declarationList.declarations[0]
      )
        continue;
    }
    if (s === registrationStatement) {
      if (found) return limit("omission-not-first-test");
      found = true;
      continue;
    }
    // Later registrations cannot execute until this synchronous callback finishes.
    // Hooks, options, top-level calls and arbitrary registration expressions are not accepted.
    if (
      found &&
      ts.isExpressionStatement(s) &&
      ts.isCallExpression(s.expression) &&
      model.nativeTest(s.expression) &&
      s.expression.arguments.length === 2 &&
      ts.isStringLiteralLike(s.expression.arguments[0]) &&
      ts.isArrowFunction(s.expression.arguments[1])
    )
      continue;
    return limit("omission-test-setup-or-earlier-registration");
  }
  if (!found) return limit("omission-registration-not-in-module");
  for (const s of production.statements) {
    if (ts.isExportDeclaration(s)) return limit("omission-production-reexport");
    if (
      ts.isImportDeclaration(s) &&
      !erasedImport(s) &&
      !(
        ts.isStringLiteralLike(s.moduleSpecifier) &&
        s.moduleSpecifier.text === "node:util"
      )
    )
      return limit("omission-production-import-outside-scope");
  }
  const original = runMockCounts(ts, fn, model, row, {
    assertion,
    module: production,
  });
  const before = original.checks.get(assertion);
  if (
    !before ||
    original.limitation ||
    before.observedCount !== before.expectedCount
  )
    return limit(
      original.limitation ?? "omission-original-count-not-established",
    );
  const calleeSource = model.location(call);
  if (!before.calls?.some((c) => c.source === calleeSource))
    return limit("omission-target-not-in-selected-history");
  const changed = runMockCounts(ts, fn, model, row, {
    assertion,
    module: production,
    omit: call,
  });
  const after = changed.checks.get(assertion);
  if (
    !after ||
    changed.limitation ||
    after.instance !== before.instance ||
    after.expectedCount !== before.expectedCount
  )
    return limit(
      changed.limitation ?? "omission-changed-count-not-established",
    );
  return {
    ...base,
    status: "source-checked",
    scope: "first-synchronous-test",
    outcome:
      after.observedCount === after.expectedCount ? "not-rejected" : "rejected",
    assertionSource: model.location(assertion),
    callSource: calleeSource,
    callbackSource: model.location(parent),
    instance: before.instance,
    expectedCount: before.expectedCount,
    originalCount: before.observedCount,
    omittedCount: after.observedCount,
  };
}
