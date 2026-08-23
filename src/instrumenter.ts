import { createHash } from "node:crypto";
import generate from "@babel/generator";
import { parse, type ParserPlugin } from "@babel/parser";
import traverse, { type NodePath } from "@babel/traverse";
import * as t from "@babel/types";
import type {
  CoverageBranchMeta,
  CoverageManifest,
  CoverageLimitation,
  CoveragePointMeta,
  McdcDecisionMeta,
} from "./types.ts";

const RUNTIME_MODULE = "virtual:supercov-runtime";
const BEGIN = "__supercovMcdcBegin";
const CONDITION = "__supercovMcdcCondition";
const END = "__supercovMcdcEnd";
const HIT = "__supercovCoverageHit";
const SELECTION_BEGIN = "__supercovSelectionBegin";
const SELECTION_RIGHT = "__supercovSelectionRight";
const SELECTION_END = "__supercovSelectionEnd";
const WITH_REQUEST_PHASE = "__supercovWithRequestPhase";
const OPTIONAL_SELECT = "__supercovOptionalSelect";
const DEFAULT_SELECTED = "__supercovDefaultSelected";
const DEFAULT_ENTERED = "__supercovDefaultEntered";
const TRY_BEGIN = "__supercovTryBegin";
const TRY_CATCH = "__supercovTryCatch";
const TRY_END = "__supercovTryEnd";
const LOOP_BEGIN = "__supercovLoopBegin";
const LOOP_ENTERED = "__supercovLoopEntered";
const LOOP_END = "__supercovLoopEnd";

function isRemixRequestHandlerName(name: string | undefined): boolean {
  return name === "loader" || name === "action";
}

function sourceFor(code: string, node: t.Node): string {
  if (
    node.start !== null &&
    node.start !== undefined &&
    node.end !== null &&
    node.end !== undefined
  ) {
    return code.slice(node.start, node.end);
  }
  return generate(node).code;
}

function stableId(
  file: string,
  kind: string,
  node: t.Node,
  suffix = "",
): string {
  return createHash("sha256")
    .update(`${file}:${kind}:${node.start ?? 0}:${node.end ?? 0}:${suffix}`)
    .digest("hex")
    .slice(0, 16);
}

function hasCompoundBooleanDecision(node: t.Expression): boolean {
  if (
    t.isLogicalExpression(node) &&
    (node.operator === "&&" || node.operator === "||")
  ) {
    return true;
  }
  return (
    t.isUnaryExpression(node, { operator: "!" }) &&
    hasCompoundBooleanDecision(node.argument)
  );
}

function collectConditions(
  node: t.Expression,
  conditions: t.Expression[],
): void {
  if (
    t.isLogicalExpression(node) &&
    (node.operator === "&&" || node.operator === "||")
  ) {
    collectConditions(node.left, conditions);
    collectConditions(node.right, conditions);
    return;
  }
  if (
    t.isUnaryExpression(node, { operator: "!" }) &&
    hasCompoundBooleanDecision(node.argument)
  ) {
    collectConditions(node.argument, conditions);
    return;
  }
  conditions.push(node);
}

function instrumentConditions(
  node: t.Expression,
  frameId: t.Identifier,
  nextIndex: { value: number },
  decisionLogicalNodes: WeakSet<t.LogicalExpression>,
  conditionRuntimeName: string,
): t.Expression {
  if (
    t.isLogicalExpression(node) &&
    (node.operator === "&&" || node.operator === "||")
  ) {
    const logical = t.logicalExpression(
      node.operator,
      instrumentConditions(
        node.left,
        frameId,
        nextIndex,
        decisionLogicalNodes,
        conditionRuntimeName,
      ),
      instrumentConditions(
        node.right,
        frameId,
        nextIndex,
        decisionLogicalNodes,
        conditionRuntimeName,
      ),
    );
    decisionLogicalNodes.add(logical);
    return logical;
  }

  if (
    t.isUnaryExpression(node, { operator: "!" }) &&
    hasCompoundBooleanDecision(node.argument)
  ) {
    return t.unaryExpression(
      "!",
      instrumentConditions(
        node.argument,
        frameId,
        nextIndex,
        decisionLogicalNodes,
        conditionRuntimeName,
      ),
      true,
    );
  }

  const index = nextIndex.value;
  nextIndex.value += 1;
  return t.callExpression(t.identifier(conditionRuntimeName), [
    t.cloneNode(frameId),
    t.numericLiteral(index),
    node,
  ]);
}

function hitStatement(
  id: string,
  hitRuntimeName: string,
): t.ExpressionStatement {
  return t.expressionStatement(
    t.callExpression(t.identifier(hitRuntimeName), [t.stringLiteral(id)]),
  );
}

function allocatedRuntimeNames(code: string): Record<string, string> {
  const preferred = [
    BEGIN,
    CONDITION,
    END,
    HIT,
    SELECTION_BEGIN,
    SELECTION_RIGHT,
    SELECTION_END,
    WITH_REQUEST_PHASE,
    OPTIONAL_SELECT,
    DEFAULT_SELECTED,
    DEFAULT_ENTERED,
    TRY_BEGIN,
    TRY_CATCH,
    TRY_END,
    LOOP_BEGIN,
    LOOP_ENTERED,
    LOOP_END,
  ];
  const names: Record<string, string> = {};
  const used = new Set<string>();
  for (const base of preferred) {
    let candidate = base;
    while (used.has(candidate) || code.includes(candidate))
      candidate = `_${candidate}`;
    names[base] = candidate;
    used.add(candidate);
  }
  return names;
}

function functionLabel(path: NodePath<t.Function>): string | undefined {
  if ("id" in path.node && t.isIdentifier(path.node.id))
    return path.node.id.name;
  const parent = path.parentPath;
  if (
    parent.isObjectProperty() ||
    parent.isObjectMethod() ||
    parent.isClassMethod()
  ) {
    const key = parent.node.key;
    if (t.isIdentifier(key)) return key.name;
    if (t.isStringLiteral(key)) return key.value;
  }
  if (parent.isVariableDeclarator() && t.isIdentifier(parent.node.id))
    return parent.node.id.name;
  return undefined;
}

function isExecutableStatement(node: t.Statement): boolean {
  if (
    t.isBlockStatement(node) ||
    t.isEmptyStatement(node) ||
    t.isFunctionDeclaration(node) ||
    node.type.startsWith("TS")
  ) {
    return false;
  }
  return !("declare" in node && node.declare === true);
}

function isAnonymousFunctionDefinition(node: t.Expression): boolean {
  return (
    t.isArrowFunctionExpression(node) ||
    (t.isFunctionExpression(node) && node.id === null) ||
    (t.isClassExpression(node) && node.id === null)
  );
}

function isSourceSensitiveFunction(path: NodePath<t.Function>): boolean {
  let child: NodePath = path;
  for (
    let parent: NodePath | null = path.parentPath;
    parent;
    parent = parent.parentPath
  ) {
    const candidate = parent.node;
    const value = child.node;
    if (
      (t.isParenthesizedExpression(candidate) ||
        t.isTSAsExpression(candidate) ||
        t.isTSTypeAssertion(candidate) ||
        t.isTSNonNullExpression(candidate)) &&
      candidate.expression === value
    ) {
      child = parent;
      continue;
    }
    if (
      (t.isConditionalExpression(candidate) &&
        (candidate.consequent === value || candidate.alternate === value)) ||
      (t.isLogicalExpression(candidate) &&
        (candidate.left === value || candidate.right === value)) ||
      (t.isSequenceExpression(candidate) &&
        candidate.expressions.at(-1) === value) ||
      (t.isAssignmentExpression(candidate) && candidate.right === value)
    ) {
      child = parent;
      continue;
    }
    if (
      ((t.isClassMethod(candidate) ||
        t.isClassProperty(candidate) ||
        t.isObjectMethod(candidate) ||
        t.isObjectProperty(candidate)) &&
        candidate.computed &&
        candidate.key === value) ||
      ((t.isMemberExpression(candidate) ||
        t.isOptionalMemberExpression(candidate)) &&
        candidate.computed &&
        candidate.property === value)
    )
      return true;
    if (
      t.isCallExpression(candidate) &&
      t.isIdentifier(candidate.callee, { name: "String" }) &&
      candidate.arguments.includes(value as t.Expression)
    )
      return true;
    if (
      t.isBinaryExpression(candidate) &&
      ["+", "<", "<=", ">", ">="].includes(candidate.operator) &&
      (candidate.left === value || candidate.right === value)
    )
      return true;
    if (
      (t.isMemberExpression(candidate) ||
        t.isOptionalMemberExpression(candidate)) &&
      !candidate.computed &&
      t.isIdentifier(candidate.property, { name: "toString" }) &&
      candidate.object === value
    )
      return true;
    // Any other operation consumes or stores the function value. Static
    // ancestry can no longer prove that this exact function is coerced.
    return false;
  }
  return false;
}

function isInsideWithStatement(path: NodePath): boolean {
  return Boolean(path.findParent((parent) => parent.isWithStatement()));
}

export interface InstrumentMcdcResult {
  code: string;
  map: ReturnType<typeof generate>["map"];
  manifest: CoverageManifest;
  decisions: McdcDecisionMeta[];
}

export function instrumentMcdc(
  code: string,
  file: string,
): InstrumentMcdcResult {
  const names = allocatedRuntimeNames(code);
  const BEGIN = names["__supercovMcdcBegin"]!;
  const CONDITION = names["__supercovMcdcCondition"]!;
  const END = names["__supercovMcdcEnd"]!;
  const HIT = names["__supercovCoverageHit"]!;
  const SELECTION_BEGIN = names["__supercovSelectionBegin"]!;
  const SELECTION_RIGHT = names["__supercovSelectionRight"]!;
  const SELECTION_END = names["__supercovSelectionEnd"]!;
  const WITH_REQUEST_PHASE = names["__supercovWithRequestPhase"]!;
  const OPTIONAL_SELECT = names["__supercovOptionalSelect"]!;
  const DEFAULT_SELECTED = names["__supercovDefaultSelected"]!;
  const DEFAULT_ENTERED = names["__supercovDefaultEntered"]!;
  const TRY_BEGIN = names["__supercovTryBegin"]!;
  const TRY_CATCH = names["__supercovTryCatch"]!;
  const TRY_END = names["__supercovTryEnd"]!;
  const LOOP_BEGIN = names["__supercovLoopBegin"]!;
  const LOOP_ENTERED = names["__supercovLoopEntered"]!;
  const LOOP_END = names["__supercovLoopEnd"]!;
  const parserPlugins: ParserPlugin[] = [
    "typescript",
    "decorators-legacy",
    "sourcePhaseImports",
    ...(file.endsWith("x") ? (["jsx"] as const) : []),
  ];
  const ast = parse(code, {
    sourceType: "unambiguous",
    sourceFilename: file,
    errorRecovery: true,
    plugins: parserPlugins,
  });
  const decisions: McdcDecisionMeta[] = [];
  const points: CoveragePointMeta[] = [];
  const branches: CoverageBranchMeta[] = [];
  const limitations: CoverageLimitation[] = [];
  const generatedStatements = new WeakSet<t.Statement>();
  const decisionLogicalNodes = new WeakSet<t.LogicalExpression>();
  let usesRequestPhaseHandler = false;

  // Instrumentation necessarily changes a function's source text. When code
  // directly observes or coerces that source, keep the entire function body
  // untouched and report the resulting coverage boundary explicitly.
  const sourceSensitiveFunctions = new WeakSet<t.Function>();
  traverse(ast, {
    Function(path) {
      if (!isSourceSensitiveFunction(path)) return;
      sourceSensitiveFunctions.add(path.node);
      if (!path.node.loc) return;
      limitations.push({
        id: stableId(file, "semantic-safety", path.node, "function-source"),
        kind: "semantic-safety",
        file,
        line: path.node.loc.start.line,
        column: path.node.loc.start.column + 1,
        source: sourceFor(code, path.node),
        reason:
          "function body is left uninstrumented because this expression observes or coerces Function source text",
      });
    },
  });
  const isWithinSourceSensitiveFunction = (path: NodePath): boolean =>
    (path.isFunction() && sourceSensitiveFunctions.has(path.node)) ||
    Boolean(
      path.findParent(
        (parent) =>
          parent.isFunction() && sourceSensitiveFunctions.has(parent.node),
      ),
    );
  traverse(ast, {
    WithStatement(path) {
      if (!path.node.loc) return;
      limitations.push({
        id: stableId(file, "semantic-safety", path.node, "with-environment"),
        kind: "semantic-safety",
        file,
        line: path.node.loc.start.line,
        column: path.node.loc.start.column + 1,
        source: sourceFor(code, path.node),
        reason:
          "with-statement body is left uninstrumented because its object environment can intercept probe identifiers",
      });
    },
  });
  const isUnsafeInstrumentationContext = (path: NodePath): boolean =>
    isWithinSourceSensitiveFunction(path) || isInsideWithStatement(path);

  // Record executable statements before adding any instrumentation statements.
  traverse(ast, {
    Statement(path) {
      const node = path.node;
      if (
        !node.loc ||
        isUnsafeInstrumentationContext(path) ||
        generatedStatements.has(node) ||
        !isExecutableStatement(node)
      )
        return;
      if (path.parentPath.isLabeledStatement()) return;

      const id = stableId(file, "statement", node);
      points.push({
        id,
        kind: "statement",
        file,
        line: node.loc.start.line,
        column: node.loc.start.column + 1,
        source: sourceFor(code, node),
      });
      const probe = hitStatement(id, HIT);
      generatedStatements.add(probe);

      if (
        path.parentPath.isProgram() ||
        path.parentPath.isBlockStatement() ||
        path.parentPath.isSwitchCase()
      ) {
        path.insertBefore(probe);
        return;
      }

      // Bare control-flow bodies become blocks. This preserves dangling-else,
      // break, continue, return, and throw semantics while giving the body an
      // independently observable statement entry.
      if (
        path.key === "body" ||
        path.key === "consequent" ||
        path.key === "alternate"
      ) {
        path.replaceWith(t.blockStatement([probe, node]));
        path.skip();
      }
    },
  });

  // Optional links are measured at the exact nullish operand. Unlike checking
  // the chain's final value, this remains accurate when a successful property
  // access or function call legitimately returns undefined.
  const instrumentOptionalOperand = (
    path: NodePath<t.Expression>,
    operand: t.Expression,
  ): t.Expression => {
    const node = path.node;
    if (!node.loc) return operand;
    const id = stableId(file, "optional-chain", node);
    const shortId = `${id}:short`;
    const continuedId = `${id}:continued`;
    branches.push({
      id,
      kind: "optional-chain",
      file,
      line: node.loc.start.line,
      column: node.loc.start.column + 1,
      source: sourceFor(code, node),
      alternatives: [
        { id: shortId, label: "nullish / short-circuited" },
        { id: continuedId, label: "non-nullish / continued" },
      ],
    });
    return t.callExpression(t.identifier(OPTIONAL_SELECT), [
      t.stringLiteral(shortId),
      t.stringLiteral(continuedId),
      operand,
    ]);
  };

  traverse(ast, {
    OptionalMemberExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      if (!path.node.optional || !t.isExpression(path.node.object)) return;
      path.node.object = instrumentOptionalOperand(
        path as unknown as NodePath<t.Expression>,
        path.node.object,
      );
    },
    OptionalCallExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      if (!path.node.optional || !t.isExpression(path.node.callee)) return;
      const callee = path.node.callee;
      if (t.isOptionalMemberExpression(callee)) {
        if (path.node.loc) {
          limitations.push({
            id: stableId(file, "semantic-safety", path.node, "optional-chain-call"),
            kind: "semantic-safety",
            file,
            line: path.node.loc.start.line,
            column: path.node.loc.start.column + 1,
            source: sourceFor(code, path.node),
            reason:
              "optional calls whose receiver is itself an optional chain are left native to preserve chain short-circuiting and method receiver semantics",
          });
        }
        return;
      }
      if (t.isMemberExpression(callee) || t.isOptionalMemberExpression(callee)) {
        // Rewriting `object.method?.()` through `.call` preserves `this` for
        // the immediate invocation but breaks the specification's optional
        // chain continuation (`object.method?.()()`). Leave it native until a
        // probe can preserve the chain's internal short-circuit target.
        if (path.node.loc) {
          limitations.push({
            id: stableId(file, "semantic-safety", path.node, "optional-method-call"),
            kind: "semantic-safety",
            file,
            line: path.node.loc.start.line,
            column: path.node.loc.start.column + 1,
            source: sourceFor(code, path.node),
            reason:
              "optional method calls are left native to preserve receiver and whole-chain short-circuit semantics",
          });
        }
        return;
      }
      path.node.callee = instrumentOptionalOperand(
        path as unknown as NodePath<t.Expression>,
        callee,
      );
    },
  });

  // Logical assignments have the same short/right split as value-selection
  // expressions, but wrapping only the RHS preserves one-time LHS evaluation.
  traverse(ast, {
    AssignmentExpression: {
      exit(path) {
        if (isUnsafeInstrumentationContext(path)) return;
        const node = path.node;
        if (
          !node.loc ||
          (node.operator !== "&&=" &&
            node.operator !== "||=" &&
            node.operator !== "??=")
        )
          return;
        const id = stableId(file, "logical-assignment", node, node.operator);
        const shortId = `${id}:short`;
        const rightId = `${id}:right`;
        branches.push({
          id,
          kind: "logical-assignment",
          file,
          line: node.loc.start.line,
          column: node.loc.start.column + 1,
          source: sourceFor(code, node),
          alternatives: [
            { id: shortId, label: "assignment skipped" },
            { id: rightId, label: "right evaluated / assigned" },
          ],
        });
        const frameId = path.scope.generateUidIdentifier("supercovSelectionFrame");
        const frameScope = path.scope.getFunctionParent() ?? path.scope.getProgramParent();
        frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
        const assignFrame = t.assignmentExpression(
          "=",
          t.cloneNode(frameId),
          t.callExpression(t.identifier(SELECTION_BEGIN), [
            t.stringLiteral(shortId),
            t.stringLiteral(rightId),
          ]),
        );
        const assignment = t.assignmentExpression(
          node.operator,
          t.cloneNode(node.left),
          t.callExpression(t.identifier(SELECTION_RIGHT), [
            t.cloneNode(frameId),
            node.right,
            ...(t.isIdentifier(node.left) &&
            !node.left.extra?.parenthesized &&
            isAnonymousFunctionDefinition(node.right)
              ? [t.stringLiteral(node.left.name)]
              : []),
          ]),
        );
        path.replaceWith(
          t.sequenceExpression([
            assignFrame,
            t.callExpression(t.identifier(SELECTION_END), [
              t.cloneNode(frameId),
              assignment,
            ]),
          ]),
        );
        path.skip();
      },
    },
  });

  // Parameter defaults execute before a function body. A tiny per-default
  // token lets the body distinguish an evaluated default from a supplied
  // value without comparing values or evaluating either expression twice.
  traverse(ast, {
    Function(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      if (!t.isBlockStatement(path.node.body)) return;
      const entries: t.Statement[] = [];
      const visitPattern = (pattern: t.Node): void => {
        if (t.isTSParameterProperty(pattern)) {
          visitPattern(pattern.parameter);
          return;
        }
        if (t.isAssignmentPattern(pattern)) {
          if (!pattern.loc) return;
          const id = stableId(file, "default-value", pattern);
          const defaultId = `${id}:default`;
          const providedId = `${id}:provided`;
          branches.push({
            id,
            kind: "default-value",
            file,
            line: pattern.loc.start.line,
            column: pattern.loc.start.column + 1,
            source: sourceFor(code, pattern),
            alternatives: [
              { id: defaultId, label: "default evaluated" },
              { id: providedId, label: "value provided" },
            ],
          });
          pattern.right = t.callExpression(t.identifier(DEFAULT_SELECTED), [
            t.stringLiteral(defaultId),
            pattern.right,
            ...(t.isIdentifier(pattern.left) &&
            isAnonymousFunctionDefinition(pattern.right)
              ? [t.stringLiteral(pattern.left.name)]
              : []),
          ]);
          entries.push(
            t.expressionStatement(
              t.callExpression(t.identifier(DEFAULT_ENTERED), [
                t.stringLiteral(defaultId),
                t.stringLiteral(providedId),
              ]),
            ),
          );
          visitPattern(pattern.left);
          return;
        }
        if (t.isRestElement(pattern)) return visitPattern(pattern.argument);
        if (t.isObjectPattern(pattern)) {
          for (const property of pattern.properties) {
            if (t.isRestElement(property)) visitPattern(property.argument);
            else visitPattern(property.value);
          }
          return;
        }
        if (t.isArrayPattern(pattern)) {
          for (const element of pattern.elements)
            if (element) visitPattern(element);
        }
      };
      for (const parameter of path.node.params) visitPattern(parameter);
      path.node.body.body.unshift(...entries);
    },
  });

  // Destructuring declarations use the same token protocol. For loop-binding
  // declarations consume the token at the start of each iteration, directly
  // after JavaScript has completed the binding operation.
  traverse(ast, {
    VariableDeclaration(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const parent = path.parentPath;
      if (parent.isForStatement() && path.key === "init") {
        let hasDefault = false;
        t.traverseFast(path.node, (node) => {
          if (t.isAssignmentPattern(node)) hasDefault = true;
        });
        const loc = path.node.loc;
        if (hasDefault && loc) {
          const node = path.node;
          limitations.push({
            id: stableId(file, "dynamic-code", node, "for-init-default"),
            kind: "dynamic-code",
            file,
            line: loc.start.line,
            column: loc.start.column + 1,
            source: sourceFor(code, node),
            reason: "destructuring defaults in a classic for initializer cannot yet be finalized without restructuring control flow",
          });
        }
        return;
      }
      const entries: t.Statement[] = [];
      const visitPattern = (pattern: t.Node): void => {
        if (t.isAssignmentPattern(pattern)) {
          if (!pattern.loc) return;
          const id = stableId(file, "default-value", pattern);
          const defaultId = `${id}:default`;
          const providedId = `${id}:provided`;
          branches.push({
            id,
            kind: "default-value",
            file,
            line: pattern.loc.start.line,
            column: pattern.loc.start.column + 1,
            source: sourceFor(code, pattern),
            alternatives: [
              { id: defaultId, label: "default evaluated" },
              { id: providedId, label: "value provided" },
            ],
          });
          pattern.right = t.callExpression(t.identifier(DEFAULT_SELECTED), [
            t.stringLiteral(defaultId),
            pattern.right,
            ...(t.isIdentifier(pattern.left) &&
            isAnonymousFunctionDefinition(pattern.right)
              ? [t.stringLiteral(pattern.left.name)]
              : []),
          ]);
          entries.push(
            t.expressionStatement(
              t.callExpression(t.identifier(DEFAULT_ENTERED), [
                t.stringLiteral(defaultId),
                t.stringLiteral(providedId),
              ]),
            ),
          );
          visitPattern(pattern.left);
          return;
        }
        if (t.isRestElement(pattern)) return visitPattern(pattern.argument);
        if (t.isObjectPattern(pattern)) {
          for (const property of pattern.properties)
            visitPattern(
              t.isRestElement(property) ? property.argument : property.value,
            );
          return;
        }
        if (t.isArrayPattern(pattern)) {
          for (const element of pattern.elements)
            if (element) visitPattern(element);
        }
      };
      for (const declaration of path.node.declarations)
        visitPattern(declaration.id);
      if (entries.length === 0) return;

      if (
        (parent.isForOfStatement() || parent.isForInStatement()) &&
        path.key === "left"
      ) {
        const body = parent.node.body;
        parent.node.body = t.isBlockStatement(body)
          ? body
          : t.blockStatement([body]);
        parent.node.body.body.unshift(...entries);
        return;
      }
      if (parent.isExportNamedDeclaration()) parent.insertAfter(entries);
      else path.insertAfter(entries);
    },
  });

  // Try/catch and enumeration loops use frames finalized in `finally`, so
  // return, break, continue, rejection, and throw paths remain observable.
  traverse(ast, {
    TryStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const node = path.node;
      if (!node.loc || !node.handler) return;
      const id = stableId(file, "try-catch", node);
      const successId = `${id}:success`;
      const catchId = `${id}:catch`;
      branches.push({
        id,
        kind: "try-catch",
        file,
        line: node.loc.start.line,
        column: node.loc.start.column + 1,
        source: "try / catch",
        alternatives: [
          { id: successId, label: "try completed without catch" },
          { id: catchId, label: "catch entered" },
        ],
      });
      const frameId = path.scope.generateUidIdentifier("supercovTryFrame");
      const frameScope = path.scope.getFunctionParent() ?? path.scope.getProgramParent();
      frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
      path.insertBefore(
        t.expressionStatement(
          t.assignmentExpression(
            "=",
            t.cloneNode(frameId),
            t.callExpression(t.identifier(TRY_BEGIN), [
              t.stringLiteral(successId),
              t.stringLiteral(catchId),
            ]),
          ),
        ),
      );
      node.handler.body.body.unshift(
        t.expressionStatement(
          t.callExpression(t.identifier(TRY_CATCH), [
            t.cloneNode(frameId),
            t.identifier("undefined"),
          ]),
        ),
      );
      const end = t.expressionStatement(
        t.callExpression(t.identifier(TRY_END), [t.cloneNode(frameId)]),
      );
      if (node.finalizer) node.finalizer.body.unshift(end);
      else node.finalizer = t.blockStatement([end]);
      path.skip();
    },
    "ForInStatement|ForOfStatement"(path: NodePath<t.ForInStatement | t.ForOfStatement>) {
      if (isUnsafeInstrumentationContext(path)) return;
      const node = path.node;
      if (!node.loc) return;
      const kind = t.isForOfStatement(node) ? "for-of" : "for-in";
      const id = stableId(file, kind, node);
      const zeroId = `${id}:zero`;
      const enteredId = `${id}:entered`;
      branches.push({
        id,
        kind,
        file,
        line: node.loc.start.line,
        column: node.loc.start.column + 1,
        source: sourceFor(code, node.right),
        alternatives: [
          { id: zeroId, label: "zero iterations" },
          { id: enteredId, label: "one or more iterations" },
        ],
      });
      const frameId = path.scope.generateUidIdentifier("supercovLoopFrame");
      const frameScope = path.scope.getFunctionParent() ?? path.scope.getProgramParent();
      frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
      const assignment = t.expressionStatement(
        t.assignmentExpression(
          "=",
          t.cloneNode(frameId),
          t.callExpression(t.identifier(LOOP_BEGIN), [
            t.stringLiteral(zeroId),
            t.stringLiteral(enteredId),
          ]),
        ),
      );
      const loop = t.cloneNode(node, true);
      loop.body = t.isBlockStatement(loop.body)
        ? loop.body
        : t.blockStatement([loop.body]);
      loop.body.body.unshift(
        t.expressionStatement(
          t.callExpression(t.identifier(LOOP_ENTERED), [
            t.cloneNode(frameId),
          ]),
        ),
      );
      let loopStatement: t.Statement = loop;
      let replacementPath: NodePath<t.Statement> = path;
      while (
        replacementPath.parentPath?.isLabeledStatement() &&
        replacementPath.parentPath.node.body === replacementPath.node
      ) {
        loopStatement = t.labeledStatement(
          t.cloneNode(replacementPath.parentPath.node.label),
          loopStatement,
        );
        replacementPath = replacementPath.parentPath;
      }
      const wrapped = t.tryStatement(
        t.blockStatement([loopStatement]),
        null,
        t.blockStatement([
          t.expressionStatement(
            t.callExpression(t.identifier(LOOP_END), [t.cloneNode(frameId)]),
          ),
        ]),
      );
      if (replacementPath === path) {
        path.insertBefore(assignment);
        path.replaceWith(wrapped);
        path.skip();
      } else {
        replacementPath.replaceWith(t.blockStatement([assignment, wrapped]));
        replacementPath.skip();
      }
    },
  });

  // Function entry is separate from statement coverage so empty functions and
  // expression-bodied arrows remain visible in the completeness denominator.
  traverse(ast, {
    Function(path) {
      const node = path.node;
      if (!node.loc || !node.body) return;
      if (isUnsafeInstrumentationContext(path)) return;
      const id = stableId(file, "function", node);
      points.push({
        id,
        kind: "function",
        file,
        line: node.loc.start.line,
        column: node.loc.start.column + 1,
        source: sourceFor(code, node),
        ...(functionLabel(path) ? { label: functionLabel(path) } : {}),
      });
      const probe = hitStatement(id, HIT);
      generatedStatements.add(probe);
      if (t.isBlockStatement(node.body)) {
        node.body.body.unshift(probe);
      } else {
        node.body = t.blockStatement([probe, t.returnStatement(node.body)]);
      }
    },
  });

  const instrumentDecision = (
    path: NodePath<t.Expression>,
    kind: McdcDecisionMeta["kind"],
  ): void => {
    if (!path.node.loc) return;
    const originalConditions: t.Expression[] = [];
    collectConditions(path.node, originalConditions);
    if (originalConditions.length === 0) return;

    const id = stableId(file, "decision", path.node, kind);
    const meta: McdcDecisionMeta = {
      id,
      file,
      line: path.node.loc.start.line,
      column: path.node.loc.start.column + 1,
      source: sourceFor(code, path.node),
      conditions: originalConditions.map((condition) =>
        sourceFor(code, condition),
      ),
      kind,
    };
    decisions.push(meta);

    // A loop predicate's path scope may be represented by Babel as the loop
    // body's block. Declaring the frame there puts it after the predicate that
    // uses it (and is especially visible in async generators). Hoist scratch
    // frames to the nearest function/program scope instead.
    const evaluationScope =
      (path.isFunctionExpression() ||
        path.isArrowFunctionExpression() ||
        path.isClassExpression()) &&
      path.scope.parent
        ? path.scope.parent
        : path.scope;
    const frameScope =
      evaluationScope.getFunctionParent() ??
      evaluationScope.getProgramParent();
    const frameId = frameScope.generateUidIdentifier("supercovMcdcFrame");
    frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
    const instrumented = instrumentConditions(
      path.node,
      frameId,
      { value: 0 },
      decisionLogicalNodes,
      CONDITION,
    );
    const begin = t.callExpression(t.identifier(BEGIN), [
      t.stringLiteral(id),
      t.valueToNode(meta),
    ]);
    const assignFrame = t.assignmentExpression(
      "=",
      t.cloneNode(frameId),
      begin,
    );
    const end = t.callExpression(t.identifier(END), [
      t.cloneNode(frameId),
      instrumented,
    ]);
    path.replaceWith(t.sequenceExpression([assignFrame, end]));
    path.skip();
  };

  traverse(ast, {
    IfStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      instrumentDecision(path.get("test"), "if");
    },
    ConditionalExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      instrumentDecision(path.get("test"), "ternary");
    },
    WhileStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      instrumentDecision(path.get("test"), "while");
    },
    DoWhileStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      instrumentDecision(path.get("test"), "do-while");
    },
    ForStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const test = path.get("test");
      if (test.node) instrumentDecision(test as NodePath<t.Expression>, "for");
    },
  });

  // A logical expression outside a control predicate selects a value or
  // render path. Record whether it short-circuited or evaluated its RHS;
  // Boolean MC/DC would be misleading when both selected values are truthy.
  traverse(ast, {
    LogicalExpression: {
      exit(path) {
        if (isUnsafeInstrumentationContext(path)) return;
        const node = path.node;
        if (!node.loc || decisionLogicalNodes.has(node)) return;
        const id = stableId(file, "logical-value", node, node.operator);
        const shortId = `${id}:short`;
        const rightId = `${id}:right`;
        branches.push({
          id,
          kind: "logical-value",
          file,
          line: node.loc.start.line,
          column: node.loc.start.column + 1,
          source: sourceFor(code, node),
          alternatives: [
            { id: shortId, label: "short-circuit / left selected" },
            { id: rightId, label: "right evaluated / selected" },
          ],
        });

        const frameId = path.scope.generateUidIdentifier(
          "supercovSelectionFrame",
        );
        const frameScope =
          path.scope.getFunctionParent() ?? path.scope.getProgramParent();
        frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
        const begin = t.callExpression(t.identifier(SELECTION_BEGIN), [
          t.stringLiteral(shortId),
          t.stringLiteral(rightId),
        ]);
        const assign = t.assignmentExpression("=", t.cloneNode(frameId), begin);
        const right = t.callExpression(t.identifier(SELECTION_RIGHT), [
          t.cloneNode(frameId),
          node.right,
        ]);
        const selection = t.logicalExpression(node.operator, node.left, right);
        const end = t.callExpression(t.identifier(SELECTION_END), [
          t.cloneNode(frameId),
          selection,
        ]);
        path.replaceWith(t.sequenceExpression([assign, end]));
        path.skip();
      },
    },
  });

  // Switch alternatives are observable independently of whether their bodies
  // are empty or fall through to another case. An implicit no-match probe must
  // live after the switch rather than in a synthetic default: a matched case
  // can legally fall through to the end and must not be counted as no-match.
  traverse(ast, {
    SwitchStatement(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const node = path.node;
      if (!node.loc) return;
      const id = stableId(file, "switch", node);
      const hasDefault = node.cases.some(
        (switchCase) => switchCase.test === null,
      );
      const enteredId = hasDefault
        ? undefined
        : path.scope.generateUidIdentifier("supercovSwitchEntered");
      const alternatives = node.cases.map((switchCase, index) => {
        const alternativeId = `${id}:case:${index}`;
        const label = switchCase.test
          ? `case ${sourceFor(code, switchCase.test)}`
          : "default";
        const probe = hitStatement(alternativeId, HIT);
        generatedStatements.add(probe);
        switchCase.consequent.unshift(
          ...(enteredId
            ? [
                t.expressionStatement(
                  t.assignmentExpression(
                    "=",
                    t.cloneNode(enteredId),
                    t.booleanLiteral(true),
                  ),
                ),
              ]
            : []),
          probe,
        );
        return { id: alternativeId, label };
      });
      let noMatchId: string | undefined;
      if (!hasDefault) {
        const alternativeId = `${id}:no-match`;
        noMatchId = alternativeId;
        alternatives.push({
          id: alternativeId,
          label: "no matching case",
        });
      }
      branches.push({
        id,
        kind: "switch",
        file,
        line: node.loc.start.line,
        column: node.loc.start.column + 1,
        source: sourceFor(code, node.discriminant),
        alternatives,
      });
      if (!enteredId || !noMatchId) return;

      let switchStatement: t.Statement = node;
      let replacementPath: NodePath<t.Statement> = path;
      while (
        replacementPath.parentPath?.isLabeledStatement() &&
        replacementPath.parentPath.node.body === replacementPath.node
      ) {
        switchStatement = t.labeledStatement(
          t.cloneNode(replacementPath.parentPath.node.label),
          switchStatement,
        );
        replacementPath = replacementPath.parentPath;
      }
      const noMatchProbe = hitStatement(noMatchId, HIT);
      generatedStatements.add(noMatchProbe);
      replacementPath.replaceWith(
        t.blockStatement([
          t.variableDeclaration("let", [
            t.variableDeclarator(
              t.cloneNode(enteredId),
              t.booleanLiteral(false),
            ),
          ]),
          switchStatement,
          t.ifStatement(
            t.unaryExpression("!", t.cloneNode(enteredId)),
            noMatchProbe,
          ),
        ]),
      );
      replacementPath.skip();
    },
  });

  // Runtime-generated source cannot be assigned a truthful static
  // denominator without parsing and instrumenting the generated program in
  // its own execution realm. Discover it and block a completeness verdict
  // instead of silently claiming 100% for only the surrounding file.
  traverse(ast, {
    CallExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const callee = path.node.callee;
      if (!path.node.loc || !t.isIdentifier(callee, { name: "eval" })) return;
      limitations.push({
        id: stableId(file, "dynamic-code", path.node, "eval"),
        kind: "dynamic-code",
        file,
        line: path.node.loc.start.line,
        column: path.node.loc.start.column + 1,
        source: sourceFor(code, path.node),
        reason: "eval-generated source has no stable pre-run coverage denominator",
      });
    },
    NewExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      if (
        !path.node.loc ||
        !t.isIdentifier(path.node.callee, { name: "Function" })
      )
        return;
      limitations.push({
        id: stableId(file, "dynamic-code", path.node, "Function"),
        kind: "dynamic-code",
        file,
        line: path.node.loc.start.line,
        column: path.node.loc.start.column + 1,
        source: sourceFor(code, path.node),
        reason: "Function-generated source has no stable pre-run coverage denominator",
      });
    },
  });

  // Route requests carry the Playwright phase as a private test-only header.
  // Wrap Remix request entry points so Node's AsyncLocalStorage can propagate
  // that exact phase through every awaited helper without application edits.
  // This runs after source instrumentation so generated wrappers never become
  // coverage obligations themselves.
  if (/^app\/routes\//.test(file)) {
    traverse(ast, {
      ExportNamedDeclaration(path) {
        const declaration = path.node.declaration;
        if (t.isVariableDeclaration(declaration)) {
          for (const declarator of declaration.declarations) {
            if (
              t.isIdentifier(declarator.id) &&
              isRemixRequestHandlerName(declarator.id.name) &&
              declarator.init
            ) {
              declarator.init = t.callExpression(
                t.identifier(WITH_REQUEST_PHASE),
                [declarator.init as t.Expression],
              );
              usesRequestPhaseHandler = true;
            }
          }
          return;
        }

        if (
          t.isFunctionDeclaration(declaration) &&
          declaration.id &&
          isRemixRequestHandlerName(declaration.id.name)
        ) {
          const exportedName = declaration.id.name;
          const originalId = path.scope.generateUidIdentifier(
            `${exportedName}CoverageOriginal`,
          );
          declaration.id = originalId;
          const wrappedExport = t.exportNamedDeclaration(
            t.variableDeclaration("const", [
              t.variableDeclarator(
                t.identifier(exportedName),
                t.callExpression(t.identifier(WITH_REQUEST_PHASE), [
                  t.cloneNode(originalId),
                ]),
              ),
            ]),
          );
          path.replaceWithMultiple([declaration, wrappedExport]);
          usesRequestPhaseHandler = true;
          return;
        }

        if (!path.node.source) return;
        const handlerSpecifiers = path.node.specifiers.filter(
          (specifier): specifier is t.ExportSpecifier =>
            t.isExportSpecifier(specifier) &&
            t.isIdentifier(specifier.exported) &&
            isRemixRequestHandlerName(specifier.exported.name),
        );
        if (handlerSpecifiers.length === 0) return;

        const replacements: Array<t.Statement | t.ModuleDeclaration> = [];
        const untouched = path.node.specifiers.filter(
          (specifier) =>
            !handlerSpecifiers.includes(specifier as t.ExportSpecifier),
        );
        if (untouched.length > 0) {
          replacements.push(
            t.exportNamedDeclaration(
              null,
              untouched,
              t.cloneNode(path.node.source),
            ),
          );
        }
        for (const specifier of handlerSpecifiers) {
          const exportedName = (specifier.exported as t.Identifier).name;
          const importedId = path.scope.generateUidIdentifier(
            `${exportedName}CoverageOriginal`,
          );
          replacements.push(
            t.importDeclaration(
              [
                t.importSpecifier(
                  t.cloneNode(importedId),
                  t.cloneNode(specifier.local),
                ),
              ],
              t.cloneNode(path.node.source),
            ),
            t.exportNamedDeclaration(
              t.variableDeclaration("const", [
                t.variableDeclarator(
                  t.identifier(exportedName),
                  t.callExpression(t.identifier(WITH_REQUEST_PHASE), [
                    t.cloneNode(importedId),
                  ]),
                ),
              ]),
            ),
          );
        }
        path.replaceWithMultiple(replacements);
        usesRequestPhaseHandler = true;
      },
    });
  }

  if (/^app\/entry\.server\.[cm]?[jt]sx?$/.test(file)) {
    traverse(ast, {
      ExportDefaultDeclaration(path) {
        const declaration = path.node.declaration;
        if (t.isFunctionDeclaration(declaration)) {
          const originalId = path.scope.generateUidIdentifier(
            "handleRequestCoverageOriginal",
          );
          declaration.id = originalId;
          path.replaceWithMultiple([
            declaration,
            t.exportDefaultDeclaration(
              t.callExpression(t.identifier(WITH_REQUEST_PHASE), [
                t.cloneNode(originalId),
              ]),
            ),
          ]);
        } else {
          path.node.declaration = t.callExpression(
            t.identifier(WITH_REQUEST_PHASE),
            [declaration as t.Expression],
          );
        }
        usesRequestPhaseHandler = true;
      },
    });
  }

  // HTTP servers and WebSocket libraries expose request-bearing callbacks
  // outside framework route exports. Wrap well-known listener boundaries and
  // let the runtime scan callback arguments for Fetch or Node request headers.
  traverse(ast, {
    CallExpression(path) {
      if (isUnsafeInstrumentationContext(path)) return;
      const callee = path.node.callee;
      const property =
        (t.isMemberExpression(callee) || t.isOptionalMemberExpression(callee)) &&
        !callee.computed &&
        t.isIdentifier(callee.property)
          ? callee.property.name
          : undefined;
      const identifier = t.isIdentifier(callee) ? callee.name : property;
      let callbackIndex = -1;
      if (
        (property === "on" || property === "once" || property === "addListener") &&
        t.isStringLiteral(path.node.arguments[0]) &&
        ["request", "upgrade", "connection"].includes(path.node.arguments[0].value)
      ) {
        callbackIndex = 1;
      } else if (identifier === "createServer") {
        for (let index = path.node.arguments.length - 1; index >= 0; index -= 1) {
          const argument = path.node.arguments[index];
          if (
            t.isFunctionExpression(argument) ||
            t.isArrowFunctionExpression(argument) ||
            t.isIdentifier(argument) ||
            t.isMemberExpression(argument)
          ) {
            callbackIndex = index;
            break;
          }
        }
      }
      if (callbackIndex < 0) return;
      const callback = path.node.arguments[callbackIndex];
      if (!callback || !t.isExpression(callback)) return;
      if (
        t.isCallExpression(callback) &&
        t.isIdentifier(callback.callee, { name: WITH_REQUEST_PHASE })
      )
        return;
      path.node.arguments[callbackIndex] = t.callExpression(
        t.identifier(WITH_REQUEST_PHASE),
        [callback],
      );
      usesRequestPhaseHandler = true;
    },
  });

  const manifest: CoverageManifest = {
    decisions,
    points,
    branches,
    ...(limitations.length > 0 ? { limitations } : {}),
  };
  if (
    decisions.length > 0 ||
    points.length > 0 ||
    branches.length > 0 ||
    usesRequestPhaseHandler
  ) {
    ast.program.body.unshift(
      t.importDeclaration(
        [
          t.importSpecifier(t.identifier(BEGIN), t.identifier("mcdcBegin")),
          t.importSpecifier(
            t.identifier(CONDITION),
            t.identifier("mcdcCondition"),
          ),
          t.importSpecifier(t.identifier(END), t.identifier("mcdcEnd")),
          t.importSpecifier(t.identifier(HIT), t.identifier("coverageHit")),
          t.importSpecifier(
            t.identifier(SELECTION_BEGIN),
            t.identifier("selectionBegin"),
          ),
          t.importSpecifier(
            t.identifier(SELECTION_RIGHT),
            t.identifier("selectionRight"),
          ),
          t.importSpecifier(
            t.identifier(SELECTION_END),
            t.identifier("selectionEnd"),
          ),
          t.importSpecifier(
            t.identifier(OPTIONAL_SELECT),
            t.identifier("optionalSelect"),
          ),
          t.importSpecifier(
            t.identifier(DEFAULT_SELECTED),
            t.identifier("defaultSelected"),
          ),
          t.importSpecifier(
            t.identifier(DEFAULT_ENTERED),
            t.identifier("defaultEntered"),
          ),
          t.importSpecifier(t.identifier(TRY_BEGIN), t.identifier("tryBegin")),
          t.importSpecifier(t.identifier(TRY_CATCH), t.identifier("tryCatch")),
          t.importSpecifier(t.identifier(TRY_END), t.identifier("tryEnd")),
          t.importSpecifier(
            t.identifier(LOOP_BEGIN),
            t.identifier("loopBegin"),
          ),
          t.importSpecifier(
            t.identifier(LOOP_ENTERED),
            t.identifier("loopEntered"),
          ),
          t.importSpecifier(t.identifier(LOOP_END), t.identifier("loopEnd")),
          ...(usesRequestPhaseHandler
            ? [
                t.importSpecifier(
                  t.identifier(WITH_REQUEST_PHASE),
                  t.identifier("withRequestPhase"),
                ),
              ]
            : []),
        ],
        t.stringLiteral(RUNTIME_MODULE),
      ),
    );
  }

  const output = generate(
    {
      ...ast,
    },
    {
      sourceMaps: true,
      sourceFileName: file,
      retainLines: true,
      comments: true,
    },
    code,
  );
  return { code: output.code, map: output.map, manifest, decisions };
}

export const mcdcRuntimeModuleId = RUNTIME_MODULE;
