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
        ts.Identifier | ts.StringLiteralLike;
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
  nativeInspect?(call: ts.CallExpression): boolean;
  nativeTty?(expr: ts.Expression): boolean;
  nativeAssertion?(call: ts.CallExpression): boolean;
  globalString?(expr: ts.Expression): boolean;
  nativeException?(
    call: ts.CallExpression,
  ): "throws" | "doesNotThrow" | undefined;
  globalError?(expr: ts.Expression): boolean;
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
  allocations?: Set<ts.ObjectLiteralExpression>;
  condition?: { node: ts.Expression; value: boolean | "invert" };
  payload?: boolean;
  originalWitness?: boolean;
  emptyMapCallback?: ts.ArrowFunction;
  directReturn?: { call: ts.CallExpression; target: ts.Expression };
  completion?: { target: ts.Statement; omit: boolean };
}

export interface CompletionCheck {
  method: "throws" | "doesNotThrow";
  callbackSource: string;
  completion: "normal" | "throw";
  throwSource?: string;
  targetEvaluations: number;
  outcome: "rejected" | "not-rejected";
  diagnostic?: {
    basis:
      | "native-error-message"
      | "own-primitive-message"
      | "absent-message"
      | "primitive-thrown-value";
    message: PayloadValue;
  };
}

export interface CompletionSensitivityEvidence {
  model: "node-first-test-completion-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  scope?: "first-synchronous-test-prefix";
  assertionSource?: string;
  targetSource?: string;
  changeText?: string;
  change?: "statement-omitted";
  original?: CompletionCheck;
  omitted?: CompletionCheck;
}

export interface DirectReturnCheck {
  predicate: string;
  actual: PayloadValue;
  expected: PayloadValue;
  callSource: string;
  targetEvaluations: number;
  outcome: "rejected" | "not-rejected";
}

export interface DirectReturnSensitivityEvidence {
  model: "node-first-test-direct-return-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  scope?: "first-synchronous-test-prefix";
  assertionSource?: string;
  targetSource?: string;
  changeSource?: string;
  changeText?: string;
  original?: DirectReturnCheck;
  variants?: {
    change:
      | "condition-true"
      | "condition-false"
      | "condition-inverted"
      | "boolean-literal-inverted";
    status: "source-checked" | "unresolved";
    reason?: string;
    check?: DirectReturnCheck;
  }[];
}

export interface PayloadValue {
  kind:
    | "undefined"
    | "null"
    | "string"
    | "number"
    | "boolean"
    | "opaque-string"
    | "quoted-string"
    | "substring-pattern"
    | "object"
    | "array";
  value?: string | number | boolean;
  properties?: { name: string; value: PayloadValue }[];
}
export interface PayloadProjection {
  instance: string;
  callSource: string;
  callIndex: number;
  argumentIndex: number;
  readAt: string;
  historySelections?: MockCountEvidence["historySelections"];
  coercion?: {
    source: string;
    rule: "string-identity" | "plain-object-default-string";
    input: PayloadValue;
  };
}
export interface PayloadCheck {
  predicate: string;
  actual: PayloadValue;
  expected: PayloadValue;
  projection: PayloadProjection;
  outcome: "rejected" | "not-rejected" | "witnessed-pass";
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
    ts.FunctionDeclaration | ts.ArrowFunction | ts.FunctionExpression;
  type Closure = {
    kind: "closure";
    node: FunctionNode;
    environment: Map<ts.Declaration, Value>;
  };
  type Aggregate = {
    kind: "object" | "array";
    properties: Map<string, Value>;
    moduleOwned: boolean;
    allocation?: ts.ObjectLiteralExpression;
    argumentSelection?: Omit<PayloadProjection, "argumentIndex" | "readAt">;
  };
  type Value =
    | Primitive
    | Mock
    | Snapshot
    | Closure
    | Aggregate
    | {
        kind: "recorded-call";
        args: Value[];
        selection: Omit<PayloadProjection, "argumentIndex" | "readAt">;
      }
    | { kind: "opaque-inspect-string" | "opaque-native-tty" }
    | { kind: "native-error"; message: string }
    | { kind: "quoted-string" | "substring-pattern"; value: string }
    | { kind: "context"; mock: Mock };
  const checks = new Map<ts.CallExpression, MockCountEvidence>();
  const payloadChecks = new Map<ts.CallExpression, PayloadCheck>();
  const directReturnChecks = new Map<ts.CallExpression, DirectReturnCheck>();
  const completionChecks = new Map<ts.CallExpression, CompletionCheck>();
  let completionTargetEvaluations = 0;
  let activeDirectCalls = 0;
  let directTargetEvaluations = 0;
  const callArguments = new WeakMap<Call, Value[]>();
  const payloadReads = new Map<ts.Expression, PayloadProjection>();
  const copyCall = (call: Call): Call => {
    const copy = { ...call };
    const args = callArguments.get(call);
    if (args) callArguments.set(copy, args);
    return copy;
  };
  const installed = new Map<string, Mock | undefined>();
  let locals = new Map<ts.Declaration, Value>(row?.bindings);
  const moduleChecks = new Map<ts.SourceFile, boolean>();
  const modules = new Map<ts.SourceFile, Map<ts.Declaration, Value>>();
  const loading = new Set<ts.SourceFile>();
  let initializing = false;
  let budget = 4096;
  class Unsupported extends Error {}
  class ReachedAssertion extends Error {}
  // A modeled language exception, never an evaluator error or control signal.
  class ProgramThrow {
    constructor(
      readonly value: Value,
      readonly source: string,
    ) {}
  }
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
      calls: mock.calls.map(copyCall),
      ...(row ? { rowBinding: row.evidence } : {}),
    },
  });
  const record = (
    mock: Mock | undefined,
    node: ts.CallExpression,
    action: string,
    args: Value[],
  ) => {
    if (mock) {
      const call = {
        source: model.location(node),
        action,
        site: model.site(node),
      };
      mock.calls.push(call);
      if (trial?.payload) callArguments.set(call, args);
    }
  };

  function describe(v: Value): PayloadValue {
    if (--budget < 0) return fail(fn, "payload-budget");
    if (v === undefined) return { kind: "undefined" };
    if (v === null) return { kind: "null" };
    if (typeof v === "string") return { kind: "string", value: v };
    if (typeof v === "boolean") return { kind: "boolean", value: v };
    if (typeof v === "number") {
      if (!Number.isFinite(v) || Object.is(v, -0))
        return fail(fn, "unsupported-payload-number");
      return { kind: "number", value: v };
    }
    if (v.kind === "opaque-inspect-string") return { kind: "opaque-string" };
    if (v.kind === "quoted-string" || v.kind === "substring-pattern")
      return { kind: v.kind, value: v.value };
    if (v.kind === "object" || v.kind === "array") {
      accessible(v, fn);
      return {
        kind: v.kind,
        properties: [...v.properties]
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([name, value]) => ({ name, value: describe(value) })),
      };
    }
    return fail(fn, "unsupported-payload-value");
  }
  function equalPayload(
    a: PayloadValue,
    b: PayloadValue,
    deep: boolean,
  ): boolean | undefined {
    if (--budget < 0) return fail(fn, "payload-budget");
    if (a.kind === "quoted-string" || b.kind === "quoted-string") {
      const other = a.kind === "quoted-string" ? b : a;
      if (other.kind === "string")
        return (other.value as string).includes("'") ? undefined : false;
      return other.kind === "opaque-string" || other.kind === "quoted-string"
        ? undefined
        : false;
    }
    if (a.kind === "opaque-string" || b.kind === "opaque-string") {
      const other = a.kind === "opaque-string" ? b : a;
      return other.kind === "string" || other.kind === "opaque-string"
        ? undefined
        : false;
    }
    if (a.kind !== b.kind) return false;
    if (a.kind === "object" || a.kind === "array") {
      if (!deep) return undefined; // SameValue requires identity, not structural equality.
      if (a.properties!.length !== b.properties!.length) return false;
      let unknown = false;
      for (let i = 0; i < a.properties!.length; i++) {
        const x = a.properties![i],
          y = b.properties![i];
        if (x.name !== y.name) return false;
        const eq = equalPayload(x.value, y.value, true);
        if (eq === false) return false;
        if (eq === undefined) unknown = true;
      }
      return unknown ? undefined : true;
    }
    return Object.is(a.value, b.value);
  }
  function substringPattern(e: ts.Expression): string | undefined {
    if (!ts.isRegularExpressionLiteral(e)) return;
    const match = /^\/([A-Za-z0-9 _:-]{1,80})\/$/.exec(e.text);
    return match?.[1]; // No flags, metacharacters, escapes or stateful regex behavior.
  }
  function independentLiteral(raw: ts.Expression): boolean {
    if (--budget < 0) return false;
    const e = peel(raw);
    if (substringPattern(e) !== undefined) return true;
    if (
      ts.isStringLiteralLike(e) ||
      ts.isNumericLiteral(e) ||
      [
        ts.SyntaxKind.NullKeyword,
        ts.SyntaxKind.TrueKeyword,
        ts.SyntaxKind.FalseKeyword,
      ].includes(e.kind)
    )
      return true;
    if (ts.isArrayLiteralExpression(e))
      return e.elements.every(independentLiteral);
    return (
      ts.isObjectLiteralExpression(e) &&
      e.properties.every(
        (p) =>
          ts.isPropertyAssignment(p) &&
          !ts.isComputedPropertyName(p.name) &&
          independentLiteral(p.initializer),
      )
    );
  }

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
      (!trial ||
        (trial.allocations &&
          (!value.allocation || !trial.allocations.has(value.allocation))))
    )
      return fail(node, "shared-module-object-history");
    return value;
  }

  function sourceValue(value: Value): boolean {
    if (--budget < 0) return fail(fn, "source-model-budget");
    return (
      primitive(value) ||
      value.kind === "closure" ||
      value.kind === "native-error" ||
      value.kind === "opaque-inspect-string" ||
      value.kind === "quoted-string" ||
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
    const testCallback =
      !!trial?.completion && target.getSourceFile() === fn.getSourceFile();
    if (
      !target.body ||
      (!model.production(target) && !testCallback) ||
      target.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword) ||
      ("asteriskToken" in target && target.asteriskToken)
    )
      return fail(at, "unsupported-call-target");
    if (!testCallback && !stableTarget(target))
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
      if (trial?.emptyMapCallback === target) return undefined;
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
    if (
      trial?.completion &&
      (ts.isNewExpression(e) || ts.isCallExpression(e)) &&
      model.globalError?.(e.expression)
    ) {
      // No custom constructor, options/cause object, coercion hooks or stack
      // inspection. Preserve the primitive-derived message for the native
      // doesNotThrow failure diagnostic, not arbitrary Error property access.
      if (e.arguments && e.arguments.length > 1)
        return fail(e, "unsupported-error-constructor-options");
      let message: Value = undefined;
      for (const arg of e.arguments ?? []) {
        message = evaluate(arg, action, depth + 1);
        if (!primitive(message))
          return fail(e, "unsupported-error-message-coercion");
      }
      return {
        kind: "native-error",
        message: message === undefined ? "" : String(message),
      };
    }
    if (trial?.directReturn?.target === e && activeDirectCalls > 0)
      directTargetEvaluations++;
    if (trial?.condition?.node === e && trial.condition.value !== "invert")
      return trial.condition.value;
    if (
      trial?.directReturn &&
      ts.isPrefixUnaryExpression(e) &&
      e.operator === ts.SyntaxKind.ExclamationToken
    ) {
      const value = evaluate(e.operand, action, depth + 1);
      if (primitive(value)) return !value;
      // ToBoolean on a fresh ordinary object/array never invokes conversion hooks.
      if (value.kind === "object" || value.kind === "array") return false;
      return fail(e, "unsupported-direct-return-truthiness");
    }
    if (ts.isTypeOfExpression(e)) {
      const v = evaluate(e.expression, action, depth + 1);
      if (primitive(v)) return typeof v;
      if (v.kind === "object" || v.kind === "array") return "object";
      if (v.kind === "closure") return "function";
      if (v.kind === "opaque-inspect-string" || v.kind === "quoted-string")
        return "string";
      return fail(e, "unsupported-abstract-typeof");
    }
    if (trial?.allocations && model.nativeTty?.(e))
      return { kind: "opaque-native-tty" };
    if (ts.isStringLiteralLike(e)) return e.text;
    if (trial?.payload && ts.isRegularExpressionLiteral(e)) {
      const value = substringPattern(e);
      if (value === undefined) return fail(e, "unsupported-payload-regexp");
      return { kind: "substring-pattern", value };
    }
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
      if (e.text === "undefined" && (!d || d.getSourceFile().isDeclarationFile))
        return undefined;
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
        if (!(
          ts.isPropertyAssignment(item) ||
          ts.isShorthandPropertyAssignment(item)
        ))
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
      return {
        kind: "object",
        properties,
        moduleOwned: initializing,
        allocation: e,
      };
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
      const result =
        e.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken
          ? left === right
          : left !== right;
      return trial?.condition?.node === e && trial.condition.value === "invert"
        ? !result
        : result;
    }
    if (ts.isPropertyAccessExpression(e)) {
      const base = evaluate(e.expression, action, depth + 1);
      if (!primitive(base)) {
        if (
          trial?.payload &&
          base.kind === "recorded-call" &&
          e.name.text === "arguments"
        )
          return {
            kind: "array",
            moduleOwned: false,
            properties: new Map(base.args.map((v, i) => [String(i), v])),
            argumentSelection: base.selection,
          };
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
    if (trial?.payload && ts.isElementAccessExpression(e)) {
      const base = evaluate(e.expression, action, depth + 1);
      const index = evaluate(e.argumentExpression, action, depth + 1);
      if (
        primitive(base) ||
        typeof index !== "number" ||
        !Number.isSafeInteger(index) ||
        index < 0
      )
        return fail(e, "unsupported-payload-index");
      if (base.kind === "history") {
        const call = base.evidence.calls?.[index],
          args = call && callArguments.get(call);
        if (!call || !args || !base.evidence.instance)
          return fail(e, "missing-selected-call");
        return {
          kind: "recorded-call",
          args,
          selection: {
            instance: base.evidence.instance,
            callSource: call.source,
            callIndex: index,
            historySelections: base.evidence.historySelections,
          },
        };
      }
      if (base.kind === "array" && base.argumentSelection) {
        if (!base.properties.has(String(index)))
          return fail(e, "missing-selected-argument");
        payloadReads.set(e, {
          ...base.argumentSelection,
          argumentIndex: index,
          readAt: model.location(e),
        });
        return accessible(base.properties.get(String(index)), e);
      }
      return fail(e, "unsupported-payload-projection");
    }
    if (!ts.isCallExpression(e)) return fail(e, "unsupported-expression");
    // The checked edit replaces this expression-bodied arrow with () => undefined.
    // Its original arguments inside the body are NOT evaluated by that edit.
    // No source is executed and no test code is modified during this query.
    if (trial?.omit === e) return undefined;
    const callee = peel(e.expression);
    if (
      trial?.directReturn &&
      ts.isPropertyAccessExpression(callee) &&
      callee.name.text === "includes"
    ) {
      const base = evaluate(callee.expression, action, depth + 1);
      if (primitive(base) || base.kind !== "array")
        return fail(e, "unsupported-direct-return-includes-receiver");
      accessible(base, e);
      const args = argumentsOf(e, action, depth + 1);
      if (
        args.length !== 1 ||
        !primitive(args[0]) ||
        [...base.properties.values()].some((value) => !primitive(value))
      )
        return fail(e, "unsupported-direct-return-includes-input");
      const sought = args[0];
      return [...base.properties.values()].some(
        (value) =>
          value === sought ||
          (typeof value === "number" &&
            typeof sought === "number" &&
            Number.isNaN(value) &&
            Number.isNaN(sought)),
      );
    }
    if (trial?.payload && model.globalString?.(callee)) {
      if (e.arguments.length !== 1)
        return fail(e, "unsupported-string-coercion-arity");
      const input = evaluate(e.arguments[0], action, depth + 1);
      let value: Value, rule: "string-identity" | "plain-object-default-string";
      if (
        typeof input === "string" ||
        (!primitive(input) &&
          (input.kind === "opaque-inspect-string" ||
            input.kind === "quoted-string"))
      ) {
        value = input;
        rule = "string-identity";
      } else if (
        !primitive(input) &&
        input.kind === "object" &&
        !input.moduleOwned &&
        !input.properties.has("toString") &&
        !input.properties.has("valueOf")
      ) {
        // Getters, custom symbols, altered prototypes and escaping writes are
        // excluded by closedCountScope. This does not execute user conversion code.
        value = "[object Object]";
        rule = "plain-object-default-string";
      } else return fail(e, "unsupported-string-coercion-input");
      const projection = payloadReads.get(peel(e.arguments[0]));
      if (projection)
        payloadReads.set(e, {
          ...projection,
          readAt: model.location(e),
          coercion: { source: model.location(e), rule, input: describe(input) },
        });
      return value;
    }
    if (trial?.allocations && model.nativeInspect?.(e)) {
      const plain = (v: Value, level = 0): boolean => {
        if (--budget < 0 || level > 32) return false;
        return (
          primitive(v) ||
          ((v.kind === "object" || v.kind === "array") &&
            !v.moduleOwned &&
            [...v.properties.values()].every((p) => plain(p, level + 1)))
        );
      };
      const args = argumentsOf(e, action, depth + 1);
      if (
        args.length !== 2 ||
        !plain(args[0]) ||
        primitive(args[1]) ||
        args[1].kind !== "object" ||
        args[1].moduleOwned
      )
        return fail(e, "unsupported-inspect-summary-input");
      const options = args[1].properties,
        colors = options.get("colors");
      if (
        options.size !== 3 ||
        options.get("depth") !== null ||
        typeof options.get("compact") !== "boolean" ||
        !(
          typeof colors === "boolean" ||
          (!primitive(colors) && colors.kind === "opaque-native-tty")
        )
      )
        return fail(e, "unsupported-inspect-summary-options");
      if (
        trial?.payload &&
        typeof args[0] === "string" &&
        /^[A-Za-z0-9 _-]{0,64}$/.test(args[0])
      )
        return { kind: "quoted-string", value: args[0] };
      return { kind: "opaque-inspect-string" };
    }
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
        const calls = base.evidence.calls!.slice(from, to).map(copyCall);
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
      record(mock, e, action, args);
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
    const exceptionMethod = trial?.completion && model.nativeException?.(e);
    if (exceptionMethod) {
      if (initializing || e.arguments.length !== 1)
        return fail(e, "unsupported-completion-assertion-shape");
      // Evaluate the operand OUTSIDE the assertion's catch. A factory's own
      // error is not a thrown result of the callback it was meant to produce.
      const callback = evaluate(e.arguments[0], action, depth + 1);
      if (primitive(callback) || callback.kind !== "closure")
        return fail(e, "completion-operand-not-source-callback");
      let thrown: ProgramThrow | undefined;
      try {
        invoke(callback, [], e, action, depth + 1);
      } catch (error) {
        if (!(error instanceof ProgramThrow)) throw error;
        thrown = error;
      }
      let diagnostic: CompletionCheck["diagnostic"];
      if (exceptionMethod === "doesNotThrow" && thrown) {
        // Native expectsNoError formats `${actual?.message}` before throwing its
        // AssertionError. An object/function conversion here can run arbitrary
        // application code, exit successfully or never return. Abrupt callback
        // completion alone is therefore insufficient proof of rejection.
        const value = thrown.value;
        if (primitive(value))
          diagnostic = {
            basis: "primitive-thrown-value",
            message: describe(undefined),
          };
        else if (value.kind === "native-error")
          diagnostic = {
            basis: "native-error-message",
            message: describe(value.message),
          };
        else if (value.kind === "closure")
          diagnostic = {
            basis: "absent-message",
            message: describe(undefined),
          };
        else if (value.kind === "object" || value.kind === "array") {
          // Accepted aggregates are fresh own-data-property objects/arrays with
          // pristine prototypes; accessors and prototype mutation are outside
          // this source model. Do not generalize this to arbitrary JS objects.
          accessible(value, e);
          const message = value.properties.get("message");
          if (!primitive(message))
            return fail(e, "completion-diagnostic-message-coercion-unresolved");
          diagnostic = {
            basis: value.properties.has("message")
              ? "own-primitive-message"
              : "absent-message",
            message: describe(message),
          };
        } else
          return fail(e, "completion-diagnostic-message-access-unresolved");
      }
      const rejected = exceptionMethod === "throws" ? !thrown : !!thrown;
      if (trial.assertion === e) {
        completionChecks.set(e, {
          method: exceptionMethod,
          callbackSource: model.location(callback.node),
          completion: thrown ? "throw" : "normal",
          ...(thrown ? { throwSource: thrown.source } : {}),
          ...(diagnostic ? { diagnostic } : {}),
          targetEvaluations: completionTargetEvaluations,
          outcome: rejected ? "rejected" : "not-rejected",
        });
        throw new ReachedAssertion();
      }
      if (rejected) return fail(e, "earlier-completion-assertion-rejects");
      return undefined;
    }
    const predicate = model.nativePredicate(e);
    if (predicate) {
      if (initializing) return fail(e, "effectful-module-initialization");
      if (
        trial &&
        predicate !== "node-same-value" &&
        !(
          (trial.payload &&
            ["node-deep-strict-equality", "node-literal-regexp"].includes(
              predicate,
            )) ||
          (trial.directReturn && predicate === "node-deep-strict-equality")
        )
      )
        return fail(e, "unsupported-trial-predicate");
      if (e.arguments.length < 2 || e.arguments.length > 3)
        return fail(e, "unsupported-comparison-arity");
      const values = e.arguments.map((arg) => evaluate(arg, action, depth + 1));
      if (values.length === 3 && !primitive(values[2]))
        return fail(e, "unsupported-assertion-message");
      const [a, b] = values;
      if (trial?.directReturn) {
        if (!independentLiteral(e.arguments[1]))
          return fail(e, "direct-return-expectation-not-independent");
        const actual = describe(a),
          expected = describe(b);
        const equal = equalPayload(
          actual,
          expected,
          predicate === "node-deep-strict-equality",
        );
        if (equal === undefined)
          return fail(e, "direct-return-predicate-undecidable-in-model");
        if (trial.assertion === e) {
          if (directTargetEvaluations === 0)
            return fail(
              e,
              "direct-return-target-not-evaluated-by-selected-call",
            );
          directReturnChecks.set(e, {
            predicate,
            actual,
            expected,
            callSource: model.location(trial.directReturn.call),
            targetEvaluations: directTargetEvaluations,
            outcome: equal ? "not-rejected" : "rejected",
          });
          throw new ReachedAssertion();
        }
        if (!equal) return fail(e, "earlier-direct-return-assertion-rejects");
        return undefined;
      }
      if (
        trial?.payload &&
        !(!primitive(a) && a.kind === "count") &&
        !(!primitive(b) && b.kind === "count")
      ) {
        const actualValue = describe(a),
          expectedValue = describe(b);
        const equal =
          predicate === "node-literal-regexp"
            ? expectedValue.kind === "substring-pattern" &&
              actualValue.kind === "string"
              ? (actualValue.value as string).includes(
                  expectedValue.value as string,
                )
              : undefined
            : equalPayload(
                actualValue,
                expectedValue,
                predicate === "node-deep-strict-equality",
              );
        const witnessed =
          equal === undefined &&
          trial.originalWitness &&
          trial.assertion === e &&
          (actualValue.kind === "opaque-string" ||
            actualValue.kind === "quoted-string") &&
          (expectedValue.kind === "string" ||
            (predicate === "node-literal-regexp" &&
              expectedValue.kind === "substring-pattern"));
        if (equal === undefined && !witnessed)
          return fail(e, "payload-predicate-undecidable-in-model");
        if (trial.assertion === e) {
          const projection = payloadReads.get(peel(e.arguments[0]));
          if (!projection || !independentLiteral(e.arguments[1]))
            return fail(e, "payload-expectation-or-projection-not-independent");
          payloadChecks.set(e, {
            predicate,
            actual: actualValue,
            expected: expectedValue,
            projection,
            outcome: witnessed
              ? "witnessed-pass"
              : equal
                ? "not-rejected"
                : "rejected",
          });
          throw new ReachedAssertion();
        }
        if (!equal) return fail(e, "earlier-payload-assertion-rejects");
        return undefined;
      }
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
      else if (trial && !Object.is(a, b))
        return fail(e, "earlier-primitive-assertion-rejects");
      if (trial?.assertion === e) throw new ReachedAssertion();
      return undefined;
    }
    const declaration = model.declaration(callee);
    const saved = declaration && locals.get(declaration);
    if (saved && !primitive(saved) && saved.kind === "mock") {
      const args = argumentsOf(e, action, depth + 1);
      if (args.some((arg) => !sourceValue(arg)))
        return fail(e, "nonprimitive-mock-argument");
      record(saved, e, action, args);
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
    const selectedDirectCall = trial?.directReturn?.call === e;
    if (selectedDirectCall) activeDirectCalls++;
    try {
      return accessible(
        invoke(target, argumentsOf(e, action, depth + 1), e, action, depth + 1),
        e,
      );
    } finally {
      if (selectedDirectCall) activeDirectCalls--;
    }
  }

  function execute(
    statement: ts.Statement,
    action: string,
    depth: number,
  ): { value: Value } | undefined {
    if (--budget < 0) return fail(statement, "source-model-budget");
    if (trial?.completion?.target === statement) {
      completionTargetEvaluations++;
      if (trial.completion.omit) return;
    }
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
    else if (
      ts.isReturnStatement(statement) &&
      (model.production(statement) || trial?.completion)
    )
      return {
        value: statement.expression
          ? evaluate(statement.expression, action, depth + 1)
          : undefined,
      };
    else if (
      ts.isIfStatement(statement) &&
      (model.production(statement) || trial?.payload || trial?.completion)
    ) {
      const condition = evaluate(statement.expression, action, depth + 1);
      if (!primitive(condition))
        return fail(statement, "nonprimitive-condition");
      const branch = condition
        ? statement.thenStatement
        : statement.elseStatement;
      if (branch) return execute(branch, action, depth + 1);
    } else if (
      ts.isBlock(statement) &&
      (model.production(statement) || trial?.payload || trial?.completion)
    ) {
      for (const child of statement.statements) {
        const returned = execute(child, action, depth + 1);
        if (returned) return returned;
      }
    } else if (trial?.completion && ts.isThrowStatement(statement)) {
      const value = evaluate(statement.expression, action, depth + 1);
      if (!sourceValue(value))
        return fail(statement, "unsupported-thrown-value");
      throw new ProgramThrow(value, model.location(statement));
    } else if (trial?.completion && ts.isTryStatement(statement)) {
      let result: { value: Value } | undefined;
      let thrown: ProgramThrow | undefined;
      try {
        result = execute(statement.tryBlock, action, depth + 1);
      } catch (error) {
        if (!(error instanceof ProgramThrow)) throw error;
        thrown = error;
      }
      if (thrown && statement.catchClause) {
        const previous = locals;
        locals = new Map(locals);
        try {
          const binding = statement.catchClause.variableDeclaration;
          if (binding)
            bind(binding.name, binding, thrown.value, action, depth + 1);
          thrown = undefined;
          result = execute(statement.catchClause.block, action, depth + 1);
        } catch (error) {
          if (!(error instanceof ProgramThrow)) throw error;
          thrown = error;
        } finally {
          locals = previous;
        }
      }
      // An evaluator limitation is NEVER a catchable JavaScript exception.
      // Nor may a finally return conceal an unsupported operation in the try.
      if (statement.finallyBlock) {
        const final = execute(statement.finallyBlock, action, depth + 1);
        if (final) return final;
      }
      if (thrown) throw thrown;
      return result;
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
    else if (error instanceof ProgramThrow)
      limitation = `uncaught-source-exception-before-selected-assertion at ${error.source}`;
    else if (!(error instanceof ReachedAssertion)) throw error;
  }
  return {
    checks,
    payloadChecks,
    directReturnChecks,
    completionChecks,
    limitation,
  };
}

/** Shared environment gate for scoped source-prefix checks. No earlier test,
 * opaque import or executable module setup can establish hidden state. */
function firstSynchronousPrefixIssue(
  ts: SyntaxAPI,
  fn: ts.ArrowFunction,
  production: ts.SourceFile,
  model: Model & { nativeTest(call: ts.CallExpression): boolean },
): string | undefined {
  const sf = fn.getSourceFile(),
    registration = fn.parent.parent;
  if (!ts.isExpressionStatement(registration) || registration.parent !== sf)
    return "registration-shape";
  let found = false;
  for (const statement of sf.statements) {
    if (ts.isImportDeclaration(statement)) {
      const c = statement.importClause;
      if (!c || !ts.isStringLiteralLike(statement.moduleSpecifier))
        return "import-outside-scope";
      if (c.isTypeOnly) continue;
      if (
        [
          "node:test",
          "node:assert/strict",
          "node:assert",
          "assert/strict",
          "assert",
        ].includes(statement.moduleSpecifier.text)
      )
        continue;
      const names = [
        c.name,
        ...(c.namedBindings && ts.isNamedImports(c.namedBindings)
          ? c.namedBindings.elements
              .filter((e) => !e.isTypeOnly)
              .map((e) => e.name)
          : []),
      ].filter((n) => n !== undefined);
      if (
        !names.length ||
        (c.namedBindings && ts.isNamespaceImport(c.namedBindings)) ||
        names.some(
          (name) => model.declaration(name)?.getSourceFile() !== production,
        )
      )
        return "import-outside-scope";
      continue;
    }
    if (
      ts.isEmptyStatement(statement) ||
      ts.isInterfaceDeclaration(statement) ||
      ts.isTypeAliasDeclaration(statement)
    )
      continue;
    if (statement === registration) {
      found = true;
      continue;
    }
    if (
      !found ||
      !ts.isExpressionStatement(statement) ||
      !ts.isCallExpression(statement.expression) ||
      !model.nativeTest(statement.expression) ||
      statement.expression.arguments.length !== 2 ||
      !ts.isStringLiteralLike(statement.expression.arguments[0]) ||
      !ts.isArrowFunction(statement.expression.arguments[1])
    )
      return "setup-or-earlier-test";
  }
  if (!found) return "registration-not-found";
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
  for (const statement of production.statements) {
    if (
      ts.isEmptyStatement(statement) ||
      ts.isInterfaceDeclaration(statement) ||
      ts.isTypeAliasDeclaration(statement)
    )
      continue;
    if (ts.isFunctionDeclaration(statement) && statement.body) continue;
    if (
      !ts.isVariableStatement(statement) ||
      !(statement.declarationList.flags & ts.NodeFlags.Const) ||
      statement.declarationList.declarations.some(
        (d) =>
          !ts.isIdentifier(d.name) ||
          !d.initializer ||
          !ts.isArrowFunction(peel(d.initializer)),
      )
    )
      return "production-initialization";
  }
}

/** Omit one complete return/throw statement and compare the actual native
 * synchronous completion predicate. Non-rejection is local to this prefix. */
export function analyzeCompletionSensitivity(
  ts: SyntaxAPI,
  fn: ts.Node,
  assertion: ts.CallExpression,
  target: ts.Node,
  model: Model & { nativeTest(call: ts.CallExpression): boolean },
): CompletionSensitivityEvidence {
  const base: CompletionSensitivityEvidence = {
    model: "node-first-test-completion-v1",
    status: "unresolved",
  };
  const limit = (reason: string) => ({ ...base, reason });
  if (
    !model.production(target) ||
    !(ts.isThrowStatement(target) || ts.isReturnStatement(target)) ||
    !ts.isArrowFunction(fn) ||
    !ts.isBlock(fn.body) ||
    fn.modifiers?.length ||
    fn.parameters.length ||
    !ts.isExpressionStatement(assertion.parent) ||
    assertion.parent.parent !== fn.body ||
    !ts.isCallExpression(fn.parent) ||
    !model.nativeTest(fn.parent) ||
    fn.parent.arguments.length !== 2 ||
    fn.parent.arguments[1] !== fn ||
    !ts.isStringLiteralLike(fn.parent.arguments[0]) ||
    !model.nativeException?.(assertion) ||
    assertion.arguments.length !== 1
  )
    return limit("completion-assertion-or-target-shape");
  const production = target.getSourceFile();
  const issue = firstSynchronousPrefixIssue(ts, fn, production, model);
  if (issue) return limit(`completion-${issue}`);
  const trial: OmissionTrial = {
    assertion,
    module: production,
    completion: { target, omit: false },
  };
  const originalRun = runMockCounts(ts, fn, model, undefined, trial);
  const original = originalRun.completionChecks.get(assertion);
  if (
    originalRun.limitation ||
    !original ||
    original.outcome !== "not-rejected" ||
    !original.targetEvaluations
  )
    return limit(
      originalRun.limitation ??
        "completion-original-unavailable-or-target-not-executed",
    );
  const omittedRun = runMockCounts(ts, fn, model, undefined, {
    ...trial,
    completion: { target, omit: true },
  });
  const omitted = omittedRun.completionChecks.get(assertion);
  if (
    omittedRun.limitation ||
    !omitted ||
    !omitted.targetEvaluations ||
    omitted.method !== original.method
  )
    return limit(omittedRun.limitation ?? "completion-omission-unavailable");
  return {
    ...base,
    status: "source-checked",
    scope: "first-synchronous-test-prefix",
    assertionSource: model.location(assertion),
    targetSource: model.location(target),
    changeText: target.getText(),
    change: "statement-omitted",
    original,
    omitted,
  };
}

/** A direct value question in a declaration-only production module and the first
 * synchronous test prefix. This shares the existing value evaluator; it does not
 * execute source, infer a witness or generalize into whole-suite survival. */
export function analyzeDirectReturnSensitivity(
  ts: SyntaxAPI,
  fn: ts.Node,
  assertion: ts.CallExpression,
  target: ts.Node,
  model: Model & { nativeTest(call: ts.CallExpression): boolean },
): DirectReturnSensitivityEvidence {
  const base: DirectReturnSensitivityEvidence = {
    model: "node-first-test-direct-return-v1",
    status: "unresolved",
  };
  const limit = (reason: string) => ({ ...base, reason });
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
  if (
    !model.production(target) ||
    !ts.isArrowFunction(fn) ||
    !ts.isBlock(fn.body) ||
    fn.modifiers?.length ||
    fn.parameters.length ||
    !ts.isExpressionStatement(assertion.parent) ||
    assertion.parent.parent !== fn.body ||
    !ts.isCallExpression(fn.parent) ||
    !model.nativeTest(fn.parent) ||
    fn.parent.arguments.length !== 2 ||
    fn.parent.arguments[1] !== fn ||
    !ts.isStringLiteralLike(fn.parent.arguments[0]) ||
    !["node-same-value", "node-deep-strict-equality"].includes(
      model.nativePredicate(assertion) ?? "",
    ) ||
    assertion.arguments.length !== 2
  )
    return limit("direct-return-assertion-shape");
  const sf = fn.getSourceFile(),
    production = target.getSourceFile();
  const scopeIssue = firstSynchronousPrefixIssue(ts, fn, production, model);
  if (scopeIssue) return limit(`direct-return-${scopeIssue}`);
  let actual = peel(assertion.arguments[0]);
  const aliases = new Set<ts.Declaration>();
  while (ts.isIdentifier(actual)) {
    const d = model.declaration(actual);
    if (
      !d ||
      !ts.isVariableDeclaration(d) ||
      aliases.has(d) ||
      !ts.isIdentifier(d.name) ||
      !(d.parent.flags & ts.NodeFlags.Const) ||
      !d.initializer ||
      d.getSourceFile() !== sf ||
      d.getStart() >= assertion.getStart()
    )
      break;
    aliases.add(d);
    actual = peel(d.initializer);
  }
  if (
    !ts.isCallExpression(actual) ||
    actual.questionDotToken ||
    !ts.isIdentifier(actual.expression) ||
    model.declaration(actual.expression)?.getSourceFile() !== production
  )
    return limit("direct-return-operand-not-source-call");
  let node = target;
  if (ts.isReturnStatement(node) && node.expression)
    node = peel(node.expression);
  if (ts.isConditionalExpression(node)) node = node.condition;
  const edits: {
    change: NonNullable<
      DirectReturnSensitivityEvidence["variants"]
    >[number]["change"];
    value: boolean | "invert";
  }[] = [];
  if (
    ts.isBinaryExpression(node) &&
    [
      ts.SyntaxKind.EqualsEqualsEqualsToken,
      ts.SyntaxKind.ExclamationEqualsEqualsToken,
    ].includes(node.operatorToken.kind)
  ) {
    edits.push(
      { change: "condition-true", value: true },
      { change: "condition-false", value: false },
      { change: "condition-inverted", value: "invert" },
    );
  } else if (
    node.kind === ts.SyntaxKind.TrueKeyword ||
    node.kind === ts.SyntaxKind.FalseKeyword
  ) {
    edits.push({
      change: "boolean-literal-inverted",
      value: node.kind !== ts.SyntaxKind.TrueKeyword,
    });
  } else return limit("direct-return-target-shape");
  const expression = node as ts.Expression;
  const trial: OmissionTrial = {
    assertion,
    module: production,
    directReturn: { call: actual, target: expression },
  };
  const originalRun = runMockCounts(ts, fn, model, undefined, trial);
  const original = originalRun.directReturnChecks.get(assertion);
  if (
    originalRun.limitation ||
    !original ||
    original.outcome !== "not-rejected"
  )
    return limit(
      originalRun.limitation ?? "direct-return-original-unavailable",
    );
  const variants = edits.map((edit) => {
    const changed = runMockCounts(ts, fn, model, undefined, {
      ...trial,
      condition: { node: expression, value: edit.value },
    });
    const check = changed.directReturnChecks.get(assertion);
    if (
      changed.limitation ||
      !check ||
      check.predicate !== original.predicate ||
      JSON.stringify(check.expected) !== JSON.stringify(original.expected) ||
      check.callSource !== original.callSource
    )
      return {
        change: edit.change,
        status: "unresolved" as const,
        reason: changed.limitation ?? "direct-return-changed-unavailable",
      };
    return { change: edit.change, status: "source-checked" as const, check };
  });
  return {
    ...base,
    status: "source-checked",
    scope: "first-synchronous-test-prefix",
    assertionSource: model.location(assertion),
    targetSource: model.location(target),
    changeSource: model.location(expression),
    changeText: expression.getText(),
    original,
    variants,
  };
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

type ScopeModel = Model & { nativeTest(call: ts.CallExpression): boolean };

/** Allocation-specific permission for a closed, synchronous, nonescaping test
 * module. This is not a general readonly inference from `const`. Native APIs,
 * isolated module loading and pristine prototypes remain model assumptions. */
function closedCountScope(
  ts: SyntaxAPI,
  sf: ts.SourceFile,
  prod: ts.SourceFile,
  model: ScopeModel,
) {
  const reject = (why: string): never => {
    throw new Error(why);
  };
  let budget = 32768;
  const walk = (n: ts.Node, f: (n: ts.Node) => void) => {
    if (--budget < 0) reject("scope-budget");
    f(n);
    ts.forEachChild(n, (c) => walk(c, f));
  };
  const peel = (raw: ts.Expression): ts.Expression => {
    let e = raw;
    while (
      ts.isParenthesizedExpression(e) ||
      ts.isAsExpression(e) ||
      ts.isSatisfiesExpression(e) ||
      ts.isNonNullExpression(e)
    )
      e = e.expression;
    return e;
  };
  const local = (n: ts.Node) => {
    const d = model.declaration(n);
    return d?.getSourceFile().isDeclarationFile ? undefined : d;
  };
  const erased = (s: ts.ImportDeclaration) => {
    const c = s.importClause;
    return (
      !!c &&
      (c.isTypeOnly ||
        (!c.name &&
          c.namedBindings &&
          ts.isNamedImports(c.namedBindings) &&
          c.namedBindings.elements.every((e) => {
            const d = local(e.name);
            return (
              e.isTypeOnly ||
              (!!d &&
                (ts.isInterfaceDeclaration(d) || ts.isTypeAliasDeclaration(d)))
            );
          })))
    );
  };
  try {
    const allocations = new Set<ts.ObjectLiteralExpression>();
    const shared = new Set<ts.VariableDeclaration>();
    const calls: ts.CallExpression[] = [];
    let factory: ts.ArrowFunction | undefined,
      factoryBinding: ts.VariableDeclaration | undefined;
    for (const s of prod.statements) {
      if (
        ts.isInterfaceDeclaration(s) ||
        ts.isTypeAliasDeclaration(s) ||
        ts.isEmptyStatement(s)
      )
        continue;
      if (ts.isImportDeclaration(s)) {
        if (erased(s)) continue;
        if (
          !ts.isStringLiteralLike(s.moduleSpecifier) ||
          s.moduleSpecifier.text !== "node:util" ||
          !s.importClause?.name ||
          s.importClause.namedBindings
        )
          reject("scope-production-import");
        continue;
      }
      if (
        !ts.isVariableStatement(s) ||
        !(s.declarationList.flags & ts.NodeFlags.Const)
      )
        return { reason: "scope-production-binding" };
      for (const d of s.declarationList.declarations) {
        if (!ts.isIdentifier(d.name) || !d.initializer)
          return { reason: "scope-production-binding" };
        const e = peel(d.initializer);
        if (ts.isObjectLiteralExpression(e)) {
          allocations.add(e);
          shared.add(d);
        }
        if (s.modifiers?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)) {
          if (factory || !ts.isArrowFunction(e))
            return { reason: "scope-single-factory" };
          factory = e;
          factoryBinding = d;
        }
      }
    }
    if (!factory || !shared.size)
      return { reason: "scope-private-allocations" };
    const bannedNames = new Set([
      "eval",
      "Function",
      "require",
      "globalThis",
      "global",
      "__proto__",
      "constructor",
      "prototype",
      "toString",
      "valueOf",
      "Symbol",
    ]);
    const noMutation = (n: ts.Node) => {
      if (
        (ts.isBinaryExpression(n) &&
          n.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
          n.operatorToken.kind <= ts.SyntaxKind.LastAssignment) ||
        ts.isDeleteExpression(n) ||
        ts.isPostfixUnaryExpression(n) ||
        (ts.isPrefixUnaryExpression(n) &&
          [ts.SyntaxKind.PlusPlusToken, ts.SyntaxKind.MinusMinusToken].includes(
            n.operator,
          )) ||
        ts.isNewExpression(n) ||
        ts.isAwaitExpression(n) ||
        ts.isYieldExpression(n) ||
        ts.isTaggedTemplateExpression(n) ||
        n.kind === ts.SyntaxKind.ThisKeyword ||
        ts.isGetAccessorDeclaration(n) ||
        ts.isSetAccessorDeclaration(n) ||
        ts.isComputedPropertyName(n) ||
        (ts.isIdentifier(n) && bannedNames.has(n.text)) ||
        ((ts.isArrowFunction(n) || ts.isFunctionExpression(n)) &&
          (!!n.modifiers?.length ||
            ("asteriskToken" in n && !!n.asteriskToken)))
      )
        reject("scope-effect-or-dynamic-escape");
    };
    walk(prod, (n) => {
      noMutation(n);
      if (ts.isCallExpression(n)) calls.push(n);
      if (ts.isElementAccessExpression(n))
        reject("scope-computed-production-read");
    });
    walk(sf, noMutation);
    const origins = (
      raw: ts.Expression | undefined,
      active = new Set<ts.Node>(),
    ): ts.ArrowFunction[] | undefined => {
      if (!raw || --budget < 0 || active.has(raw)) return;
      const e = peel(raw),
        next = new Set(active).add(raw);
      if (ts.isArrowFunction(e)) return [e];
      if (ts.isIdentifier(e)) {
        const d = local(e);
        if (
          d &&
          ts.isVariableDeclaration(d) &&
          d.initializer &&
          d.parent.flags & ts.NodeFlags.Const
        )
          return origins(d.initializer, next);
        if (
          d &&
          ts.isBindingElement(d) &&
          ts.isObjectBindingPattern(d.parent) &&
          ts.isParameter(d.parent.parent)
        ) {
          const p = d.parent.parent,
            owner = p.parent,
            binding = owner.parent;
          if (
            !ts.isArrowFunction(owner) ||
            owner === factory ||
            !ts.isVariableDeclaration(binding) ||
            binding.initializer !== owner
          )
            return;
          let direct = true;
          walk(prod, (n) => {
            if (
              ts.isIdentifier(n) &&
              local(n) === binding &&
              n !== binding.name &&
              !(ts.isCallExpression(n.parent) && n.parent.expression === n)
            )
              direct = false;
          });
          if (!direct) return;
          const sites = calls.filter((c) => local(c.expression) === binding),
            found: ts.ArrowFunction[] = [];
          if (!sites.length) return;
          const key = d.propertyName ?? d.name;
          if (!(ts.isIdentifier(key) || ts.isStringLiteralLike(key))) return;
          for (const c of sites) {
            const arg =
              c.arguments[owner.parameters.indexOf(p)] ?? p.initializer;
            if (!arg) return;
            const obj = peel(arg);
            if (!ts.isObjectLiteralExpression(obj)) return;
            let value = d.initializer;
            const names = new Set<string>();
            for (const member of obj.properties) {
              if (
                !ts.isPropertyAssignment(member) ||
                !(
                  ts.isIdentifier(member.name) ||
                  ts.isStringLiteralLike(member.name)
                ) ||
                names.has(member.name.text)
              )
                return;
              names.add(member.name.text);
              if (member.name.text === key.text) value = member.initializer;
            }
            const targets = origins(value, next);
            if (!targets) return;
            found.push(...targets);
          }
          return found;
        }
      }
      if (ts.isCallExpression(e)) {
        const targets = origins(e.expression, next),
          result: ts.ArrowFunction[] = [];
        if (!targets) return;
        for (const f of targets) {
          let body: ts.Expression | undefined;
          if (!ts.isBlock(f.body)) body = f.body;
          else if (
            f.body.statements.length === 1 &&
            ts.isReturnStatement(f.body.statements[0])
          )
            body = f.body.statements[0].expression;
          const target = origins(body, next);
          if (!target) return;
          result.push(...target);
        }
        return result;
      }
      return;
    };
    const methods = new Set<string>(),
      sharedMethods = new Set<ts.ArrowFunction>();
    for (const obj of allocations)
      for (const prop of obj.properties) {
        if (!ts.isPropertyAssignment(prop) || !ts.isIdentifier(prop.name))
          return { reason: "scope-shared-member" };
        const targets = origins(prop.initializer);
        if (!targets?.length) return { reason: "scope-method-origin" };
        methods.add(prop.name.text);
        targets.forEach((t) => sharedMethods.add(t));
      }
    const callOrigins = new Map(calls.map((c) => [c, origins(c.expression)]));
    const nativeMap = (c: ts.CallExpression) => {
      if (
        !ts.isPropertyAccessExpression(c.expression) ||
        c.expression.name.text !== "map" ||
        c.arguments.length !== 1 ||
        !ts.isArrowFunction(c.arguments[0])
      )
        return false;
      const p = local(c.expression.expression);
      if (
        !p ||
        !ts.isParameter(p) ||
        !ts.isIdentifier(p.name) ||
        !ts.isArrowFunction(p.parent)
      )
        return false;
      const owner = p.parent;
      if (owner === factory || sharedMethods.has(owner)) return false;
      const sites = calls.filter((call) =>
        callOrigins.get(call)?.includes(owner),
      );
      return (
        sites.length > 0 &&
        sites.every((call) => {
          const arg = call.arguments[owner.parameters.indexOf(p)];
          if (!arg) return false;
          const e = peel(arg),
            d = local(e);
          return (
            ts.isArrayLiteralExpression(e) ||
            (!!d &&
              ts.isParameter(d) &&
              !!d.dotDotDotToken &&
              ts.isIdentifier(d.name))
          );
        })
      );
    };
    walk(prod, (n) => {
      if (
        ts.isIdentifier(n) &&
        shared.has(local(n) as ts.VariableDeclaration) &&
        n !== (local(n) as ts.VariableDeclaration).name
      ) {
        let p: ts.Node = n;
        while (
          ts.isParenthesizedExpression(p.parent) ||
          ts.isConditionalExpression(p.parent)
        )
          p = p.parent;
        if (!ts.isReturnStatement(p.parent))
          reject("scope-shared-object-escape");
        let owner: ts.Node = p.parent;
        while (owner.parent && !ts.isArrowFunction(owner)) owner = owner.parent;
        if (owner !== factory) reject("scope-shared-object-escape");
      }
      if (
        ts.isIdentifier(n) &&
        local(n) === factoryBinding &&
        n !== factoryBinding?.name
      )
        reject("scope-factory-reentry");
      if (ts.isCallExpression(n)) {
        const e = n.expression;
        const console =
          ts.isPropertyAccessExpression(e) &&
          model.globalConsole(e.expression) &&
          ["log", "error"].includes(e.name.text);
        if (
          !callOrigins.get(n)?.length &&
          !console &&
          !model.nativeInspect?.(n) &&
          !nativeMap(n)
        )
          reject("scope-call-origin");
      }
    });
    const callbacks: ts.ArrowFunction[] = [],
      tables = new Set<ts.VariableDeclaration>();
    const register = (s: ts.Statement) => {
      if (!ts.isExpressionStatement(s) || !ts.isCallExpression(s.expression))
        return reject("scope-registration");
      const c = s.expression,
        f = c.arguments[1];
      if (
        !model.nativeTest(c) ||
        c.arguments.length !== 2 ||
        !ts.isArrowFunction(f) ||
        !ts.isBlock(f.body) ||
        f.parameters.length !== 1 ||
        !ts.isIdentifier(f.parameters[0].name) ||
        f.parameters[0].initializer ||
        f.parameters[0].dotDotDotToken
      )
        return reject("scope-registration");
      const rows = sourceTestRows(ts, f, model);
      if (rows?.reason || (!rows && !ts.isStringLiteralLike(c.arguments[0])))
        return reject("scope-row-registration");
      if (rows) {
        const loop = c.parent.parent.parent;
        if (!ts.isForOfStatement(loop)) return reject("scope-row-registration");
        const table = local(loop.expression);
        if (!table || !ts.isVariableDeclaration(table))
          return reject("scope-row-registration");
        tables.add(table);
      }
      callbacks.push(f);
    };
    // Check registrations first so only their proven exclusive literal tables are allowed.
    for (const s of sf.statements) {
      if (ts.isExpressionStatement(s)) register(s);
      else if (ts.isForOfStatement(s)) {
        if (!ts.isBlock(s.statement) || s.statement.statements.length !== 1)
          return { reason: "scope-row-registration" };
        register(s.statement.statements[0]);
      }
    }
    for (const s of sf.statements) {
      if (ts.isImportDeclaration(s)) {
        if (erased(s)) continue;
        if (!ts.isStringLiteralLike(s.moduleSpecifier) || !s.importClause)
          return { reason: "scope-test-import" };
        if (
          [
            "node:test",
            "node:assert/strict",
            "node:assert",
            "assert/strict",
            "assert",
          ].includes(s.moduleSpecifier.text)
        )
          continue;
        const names = s.importClause.namedBindings;
        if (
          s.importClause.name ||
          !names ||
          !ts.isNamedImports(names) ||
          names.elements.some((e) => local(e.name) !== factoryBinding)
        )
          return { reason: "scope-test-import" };
      } else if (ts.isVariableStatement(s)) {
        if (s.declarationList.declarations.some((d) => !tables.has(d)))
          return { reason: "scope-test-setup" };
      } else if (
        !ts.isExpressionStatement(s) &&
        !ts.isForOfStatement(s) &&
        !ts.isEmptyStatement(s)
      )
        return { reason: "scope-test-setup" };
    }
    const literal = (raw: ts.Expression): boolean => {
      if (--budget < 0) return false;
      const e = peel(raw);
      if (
        ts.isStringLiteralLike(e) ||
        ts.isNumericLiteral(e) ||
        [
          ts.SyntaxKind.TrueKeyword,
          ts.SyntaxKind.FalseKeyword,
          ts.SyntaxKind.NullKeyword,
        ].includes(e.kind)
      )
        return true;
      if (ts.isArrayLiteralExpression(e)) return e.elements.every(literal);
      return (
        ts.isObjectLiteralExpression(e) &&
        e.properties.every(
          (p) =>
            ts.isPropertyAssignment(p) &&
            ts.isIdentifier(p.name) &&
            literal(p.initializer),
        )
      );
    };
    for (const fn of callbacks) {
      const receivers = new Set<ts.VariableDeclaration>(),
        mocks = new Set<ts.VariableDeclaration>();
      const row = sourceTestRows(ts, fn, model)?.rows[0];
      walk(fn.body, (n) => {
        if (!ts.isCallExpression(n)) return;
        if (local(n.expression) === factoryBinding) {
          const arg = n.arguments[0];
          if (
            n.arguments.length !== 1 ||
            !ts.isObjectLiteralExpression(arg) ||
            !arg.properties.every(
              (p) =>
                (ts.isPropertyAssignment(p) &&
                  ts.isIdentifier(p.name) &&
                  literal(p.initializer)) ||
                (ts.isShorthandPropertyAssignment(p) &&
                  !p.objectAssignmentInitializer &&
                  !!row?.bindings.has(local(p.name)!)),
            )
          )
            reject("scope-factory-input");
          if (
            !ts.isVariableDeclaration(n.parent) ||
            !ts.isIdentifier(n.parent.name) ||
            !(n.parent.parent.flags & ts.NodeFlags.Const)
          )
            return reject("scope-receiver-binding");
          receivers.add(n.parent);
        }
        if (model.nativeMock(n)) {
          const [receiver, method, replacement] = n.arguments;
          if (
            n.arguments.length !== 3 ||
            !model.globalConsole(receiver) ||
            !ts.isStringLiteralLike(method) ||
            !["log", "error"].includes(method.text) ||
            !ts.isArrowFunction(replacement) ||
            replacement.parameters.length ||
            !ts.isBlock(replacement.body) ||
            replacement.body.statements.length ||
            !ts.isVariableDeclaration(n.parent)
          )
            reject("scope-mock-installation");
          if (ts.isVariableDeclaration(n.parent)) mocks.add(n.parent);
        }
      });
      walk(fn.body, (n) => {
        if (
          ts.isIdentifier(n) &&
          receivers.has(local(n) as ts.VariableDeclaration) &&
          n !== (local(n) as ts.VariableDeclaration).name
        ) {
          const prop = n.parent,
            call = prop.parent;
          if (
            !ts.isPropertyAccessExpression(prop) ||
            prop.expression !== n ||
            !methods.has(prop.name.text) ||
            !ts.isCallExpression(call) ||
            call.expression !== prop ||
            !call.arguments.every(literal)
          )
            reject("scope-receiver-escape");
        }
        if (
          ts.isIdentifier(n) &&
          local(n) === factoryBinding &&
          !(ts.isCallExpression(n.parent) && n.parent.expression === n)
        )
          reject("scope-factory-escape");
        if (!ts.isCallExpression(n)) return;
        const e = n.expression;
        const source =
          ts.isPropertyAccessExpression(e) &&
          receivers.has(local(e.expression) as ts.VariableDeclaration);
        let root: ts.Expression = e;
        while (ts.isPropertyAccessExpression(root)) root = root.expression;
        const mockRead =
          mocks.has(local(root) as ts.VariableDeclaration) &&
          ts.isPropertyAccessExpression(e) &&
          ["callCount", "slice"].includes(e.name.text);
        if (
          !source &&
          !mockRead &&
          !model.nativeMock(n) &&
          !model.nativeAssertion?.(n) &&
          !model.globalString?.(e) &&
          local(e) !== factoryBinding
        )
          reject("scope-test-call-origin");
      });
    }
    return { allocations, callbacks };
  } catch (error) {
    return {
      reason: error instanceof Error ? error.message : "scope-unavailable",
    };
  }
}

export interface CountSensitivityEvidence {
  model: "node-closed-count-sensitivity-v1";
  status: "source-checked" | "unresolved";
  reason?: string;
  scope?: "closed-synchronous-test-module";
  assertionSource?: string;
  targetSource?: string;
  conditionSource?: string;
  conditionText?: string;
  allocations?: string[];
  instance?: string;
  expectedCount?: number;
  originalCount?: number;
  variants?: {
    change: "condition-true" | "condition-false" | "condition-inverted";
    status: "source-checked" | "unresolved";
    reason?: string;
    count?: number;
    outcome?: "rejected" | "not-rejected";
  }[];
}

/** A finite, explicit control-change question, not protection against arbitrary edits. */
export function analyzeCountSensitivity(
  ts: SyntaxAPI,
  fn: ts.Node,
  assertion: ts.CallExpression,
  target: ts.Node,
  model: ScopeModel,
  row?: BoundRow,
): CountSensitivityEvidence {
  const base: CountSensitivityEvidence = {
    model: "node-closed-count-sensitivity-v1",
    status: "unresolved",
  };
  const limit = (reason: string) => ({ ...base, reason });
  const production = target.getSourceFile();
  if (
    !model.production(target) ||
    !ts.isArrowFunction(fn) ||
    !ts.isBlock(fn.body) ||
    assertion.parent.parent !== fn.body ||
    model.nativePredicate(assertion) !== "node-same-value"
  )
    return limit("count-sensitivity-assertion-shape");
  let condition: ts.Node = target;
  if (ts.isReturnStatement(condition) && condition.expression)
    condition = condition.expression;
  if (ts.isConditionalExpression(condition)) condition = condition.condition;
  if (ts.isIfStatement(condition)) condition = condition.expression;
  // Exact primitive equality, without getters, calls, overloaded coercion or side effects.
  if (
    !ts.isBinaryExpression(condition) ||
    ![
      ts.SyntaxKind.EqualsEqualsEqualsToken,
      ts.SyntaxKind.ExclamationEqualsEqualsToken,
    ].includes(condition.operatorToken.kind) ||
    ![condition.left, condition.right].every(
      (e) =>
        ts.isIdentifier(e) ||
        ts.isStringLiteralLike(e) ||
        ts.isNumericLiteral(e),
    )
  )
    return limit("count-sensitivity-condition-shape");
  const scope = closedCountScope(ts, fn.getSourceFile(), production, model);
  if (!scope.allocations || !scope.callbacks?.includes(fn))
    return limit(scope.reason ?? "count-sensitivity-scope");
  const trial = {
    assertion,
    module: production,
    allocations: scope.allocations,
  };
  const original = runMockCounts(ts, fn, model, row, trial),
    before = original.checks.get(assertion);
  if (
    !before ||
    original.limitation ||
    before.observedCount !== before.expectedCount
  )
    return limit(
      original.limitation ?? "count-sensitivity-original-unavailable",
    );
  const variants: NonNullable<CountSensitivityEvidence["variants"]> = [];
  for (const value of [true, false, "invert"] as const) {
    const changed = runMockCounts(ts, fn, model, row, {
      ...trial,
      condition: { node: condition, value },
    });
    const after = changed.checks.get(assertion),
      change =
        value === true
          ? "condition-true"
          : value === false
            ? "condition-false"
            : "condition-inverted";
    if (
      changed.limitation ||
      !after ||
      after.instance !== before.instance ||
      after.expectedCount !== before.expectedCount
    )
      variants.push({
        change,
        status: "unresolved",
        reason: changed.limitation ?? "count-sensitivity-changed-unavailable",
      });
    else
      variants.push({
        change,
        status: "source-checked",
        count: after.observedCount,
        outcome:
          after.observedCount === after.expectedCount
            ? "not-rejected"
            : "rejected",
      });
  }
  return {
    ...base,
    status: "source-checked",
    scope: "closed-synchronous-test-module",
    assertionSource: model.location(assertion),
    targetSource: model.location(target),
    conditionSource: model.location(condition),
    conditionText: condition.getText(),
    allocations: [...scope.allocations].map(model.location),
    instance: before.instance,
    expectedCount: before.expectedCount,
    originalCount: before.observedCount,
    variants,
  };
}

export interface PayloadSensitivityEvidence {
  model: "node-closed-payload-sensitivity-v2";
  status: "source-checked" | "unresolved";
  reason?: string;
  scope?: "closed-synchronous-test-module";
  assertionSource?: string;
  targetSource?: string;
  changeSource?: string;
  changeText?: string;
  allocations?: string[];
  original?: PayloadCheck;
  variants?: {
    change:
      | "condition-true"
      | "condition-false"
      | "condition-inverted"
      | "map-callback-empty";
    status: "source-checked" | "unresolved";
    reason?: string;
    check?: PayloadCheck;
  }[];
}

/** Specific control or native-map callback edits, evaluated only up to an
 * existing selected-argument predicate. No runtime values or oracle labels enter. */
export function analyzePayloadSensitivity(
  ts: SyntaxAPI,
  fn: ts.Node,
  assertion: ts.CallExpression,
  target: ts.Node,
  model: ScopeModel,
  row?: BoundRow,
): PayloadSensitivityEvidence {
  const base: PayloadSensitivityEvidence = {
    model: "node-closed-payload-sensitivity-v2",
    status: "unresolved",
  };
  const limit = (reason: string) => ({ ...base, reason });
  if (
    !model.production(target) ||
    !ts.isArrowFunction(fn) ||
    !ts.isBlock(fn.body) ||
    ![
      "node-same-value",
      "node-deep-strict-equality",
      "node-literal-regexp",
    ].includes(model.nativePredicate(assertion) ?? "")
  )
    return limit("payload-sensitivity-assertion-shape");
  let ancestor: ts.Node = assertion.parent;
  while (ancestor !== fn && !ts.isSourceFile(ancestor)) {
    if (ts.isArrowFunction(ancestor) || ts.isFunctionExpression(ancestor))
      return limit("payload-nested-assertion");
    ancestor = ancestor.parent;
  }
  if (ancestor !== fn) return limit("payload-assertion-outside-test");
  const scope = closedCountScope(
    ts,
    fn.getSourceFile(),
    target.getSourceFile(),
    model,
  );
  if (!scope.allocations || !scope.callbacks?.includes(fn))
    return limit(scope.reason ?? "payload-sensitivity-scope");
  let node = target;
  if (ts.isReturnStatement(node) && node.expression) node = node.expression;
  if (ts.isConditionalExpression(node)) node = node.condition;
  const edits: {
    change: NonNullable<
      PayloadSensitivityEvidence["variants"]
    >[number]["change"];
    trial: Partial<OmissionTrial>;
  }[] = [];
  let changeNode: ts.Node = node;
  if (
    ts.isBinaryExpression(node) &&
    [
      ts.SyntaxKind.EqualsEqualsEqualsToken,
      ts.SyntaxKind.ExclamationEqualsEqualsToken,
    ].includes(node.operatorToken.kind) &&
    [node.left, node.right].every(
      (e) =>
        ts.isIdentifier(e) ||
        ts.isStringLiteralLike(e) ||
        ts.isNumericLiteral(e) ||
        (ts.isTypeOfExpression(e) && ts.isIdentifier(e.expression)),
    )
  ) {
    for (const value of [true, false, "invert"] as const)
      edits.push({
        change:
          value === true
            ? "condition-true"
            : value === false
              ? "condition-false"
              : "condition-inverted",
        trial: { condition: { node, value } },
      });
  } else if (
    ts.isCallExpression(node) &&
    ts.isPropertyAccessExpression(node.expression) &&
    node.expression.name.text === "map" &&
    node.arguments.length === 1 &&
    ts.isArrowFunction(node.arguments[0]) &&
    ts.isBlock(node.arguments[0].body)
  ) {
    // Native array origin is established by scope and the interpreter, not spelling.
    changeNode = node.arguments[0].body;
    edits.push({
      change: "map-callback-empty",
      trial: { emptyMapCallback: node.arguments[0] },
    });
  } else return limit("payload-sensitivity-target-shape");
  const trial: OmissionTrial = {
    assertion,
    module: target.getSourceFile(),
    allocations: scope.allocations,
    payload: true,
  };
  const originalRun = runMockCounts(ts, fn, model, row, {
      ...trial,
      originalWitness: true,
    }),
    original = originalRun.payloadChecks.get(assertion);
  if (
    originalRun.limitation ||
    !original ||
    !["not-rejected", "witnessed-pass"].includes(original.outcome)
  )
    return limit(
      originalRun.limitation ?? "payload-original-predicate-unavailable",
    );
  const variants: NonNullable<PayloadSensitivityEvidence["variants"]> =
    edits.map((edit) => {
      const run = runMockCounts(ts, fn, model, row, {
          ...trial,
          ...edit.trial,
        }),
        check = run.payloadChecks.get(assertion);
      if (
        run.limitation ||
        !check ||
        check.predicate !== original.predicate ||
        JSON.stringify(check.expected) !== JSON.stringify(original.expected) ||
        check.projection.instance !== original.projection.instance ||
        check.projection.argumentIndex !== original.projection.argumentIndex ||
        check.projection.readAt !== original.projection.readAt
      )
        return {
          change: edit.change,
          status: "unresolved",
          reason: run.limitation ?? "payload-changed-predicate-unavailable",
        };
      return { change: edit.change, status: "source-checked", check };
    });
  return {
    ...base,
    status: "source-checked",
    scope: "closed-synchronous-test-module",
    assertionSource: model.location(assertion),
    targetSource: model.location(target),
    changeSource: model.location(changeNode),
    changeText: changeNode.getText(),
    allocations: [...scope.allocations].map(model.location),
    original,
    variants,
  };
}
