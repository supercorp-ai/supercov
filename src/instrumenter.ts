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
const REGISTER_V2 = "__supercovRegisterProbeV2";
const END_V2 = "__supercovMcdcEndV2";
const HIT_V2 = "__supercovCoverageHitV2";
const FILE_V2 = "__supercovProbeFileV2";
const CLOCK_V2 = "__supercovProbeClockV2";
const HITS_V2 = "__supercovProbeHitsV2";
const DECISIONS_V2 = "__supercovProbeDecisionsV2";
const COMPLETE_V2 = "__supercovProbeCompleteV2";
const SELECTION_BEGIN = "__supercovSelectionBegin";
const SELECTION_RIGHT = "__supercovSelectionRight";
const SELECTION_END = "__supercovSelectionEnd";
const WITH_REQUEST_PHASE = "__supercovWithRequestPhase";
const OPTIONAL_SELECT = "__supercovOptionalSelect";
const OPTIONAL_CALL_BEGIN = "__supercovOptionalCallBegin";
const OPTIONAL_CALL_REACHED = "__supercovOptionalCallReached";
const OPTIONAL_CALL_CONTINUED = "__supercovOptionalCallContinued";
const OPTIONAL_CALL_END = "__supercovOptionalCallEnd";
const DEFAULT_SELECTED = "__supercovDefaultSelected";
const DEFAULT_ENTERED = "__supercovDefaultEntered";
const TRY_BEGIN = "__supercovTryBegin";
const TRY_CATCH = "__supercovTryCatch";
const TRY_END = "__supercovTryEnd";
const LOOP_BEGIN = "__supercovLoopBegin";
const LOOP_ENTERED = "__supercovLoopEntered";
const LOOP_END = "__supercovLoopEnd";

function isRequestHandlerName(file: string, name: string | undefined): boolean {
  if (name === "loader" || name === "action")
    return /^app\/routes\//.test(file);
  return (
    /(?:^|\/)app\/.*\/route\.[cm]?[jt]sx?$/.test(file) &&
    ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"].includes(
      name ?? "",
    )
  );
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

function restoreClonedOffsets(original: t.Node, clone: t.Node): void {
  clone.start = original.start;
  clone.end = original.end;
  const keys = t.VISITOR_KEYS[original.type] ?? [];
  for (const key of keys) {
    const originalChild = (original as unknown as Record<string, unknown>)[key];
    const clonedChild = (clone as unknown as Record<string, unknown>)[key];
    if (Array.isArray(originalChild) && Array.isArray(clonedChild)) {
      for (let index = 0; index < originalChild.length; index += 1) {
        const sourceNode = originalChild[index];
        const targetNode = clonedChild[index];
        if (t.isNode(sourceNode) && t.isNode(targetNode))
          restoreClonedOffsets(sourceNode, targetNode);
      }
    } else if (t.isNode(originalChild) && t.isNode(clonedChild)) {
      restoreClonedOffsets(originalChild, clonedChild);
    }
  }
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

function probeV2ReachableVectorCount(
  node: t.Expression,
  conditions: t.Expression[],
): number {
  const indices = new WeakMap<t.Expression, number>();
  for (const [index, condition] of conditions.entries())
    indices.set(condition, index);
  const vectors = new Set<number>();

  const evaluate = (
    expression: t.Expression,
    assignment: number,
    encoded: { value: number },
  ): boolean => {
    if (
      t.isLogicalExpression(expression) &&
      (expression.operator === "&&" || expression.operator === "||")
    ) {
      const left = evaluate(expression.left, assignment, encoded);
      return expression.operator === "&&"
        ? left && evaluate(expression.right, assignment, encoded)
        : left || evaluate(expression.right, assignment, encoded);
    }
    if (
      t.isUnaryExpression(expression, { operator: "!" }) &&
      hasCompoundBooleanDecision(expression.argument)
    )
      return !evaluate(expression.argument, assignment, encoded);

    const index = indices.get(expression);
    if (index === undefined)
      throw new Error("probe v2 condition index missing");
    const value = (assignment & (1 << index)) !== 0;
    encoded.value += (value ? 2 : 1) * 3 ** index;
    return value;
  };

  for (
    let assignment = 0;
    assignment < 2 ** conditions.length;
    assignment += 1
  ) {
    const encoded = { value: 0 };
    const outcome = evaluate(node, assignment, encoded);
    vectors.add(encoded.value * 2 + (outcome ? 1 : 0));
  }
  return vectors.size;
}

function markDecisionLogicalNodes(
  node: t.Expression,
  decisions: WeakSet<t.LogicalExpression>,
): void {
  if (
    t.isLogicalExpression(node) &&
    (node.operator === "&&" || node.operator === "||")
  ) {
    decisions.add(node);
    markDecisionLogicalNodes(node.left, decisions);
    markDecisionLogicalNodes(node.right, decisions);
    return;
  }
  if (
    t.isUnaryExpression(node, { operator: "!" }) &&
    hasCompoundBooleanDecision(node.argument)
  )
    markDecisionLogicalNodes(node.argument, decisions);
}

function hasNestedCoverageSurface(node: t.Node): boolean {
  let found = false;
  const visit = (candidate: t.Node): void => {
    if (found) return;
    if (
      t.isLogicalExpression(candidate) ||
      t.isConditionalExpression(candidate) ||
      t.isOptionalMemberExpression(candidate) ||
      t.isOptionalCallExpression(candidate) ||
      t.isFunction(candidate) ||
      t.isClassExpression(candidate) ||
      (t.isAssignmentExpression(candidate) &&
        ["&&=", "||=", "??="].includes(candidate.operator))
    ) {
      found = true;
      return;
    }
    for (const key of t.VISITOR_KEYS[candidate.type] ?? []) {
      const child = (candidate as unknown as Record<string, unknown>)[key];
      if (Array.isArray(child)) {
        for (const item of child)
          if (item && typeof item === "object" && "type" in item)
            visit(item as t.Node);
      } else if (child && typeof child === "object" && "type" in child) {
        visit(child as t.Node);
      }
    }
  };
  visit(node);
  return found;
}

function isCoverageTransparentDecision(conditions: t.Expression[]): boolean {
  return conditions.every((condition) => !hasNestedCoverageSurface(condition));
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

function instrumentConditionsV2(
  node: t.Expression,
  frameId: t.Identifier,
  conditionTemps: t.Identifier[],
  nextIndex: { value: number },
  decisionLogicalNodes: WeakSet<t.LogicalExpression>,
): t.Expression {
  if (
    t.isLogicalExpression(node) &&
    (node.operator === "&&" || node.operator === "||")
  ) {
    const logical = t.logicalExpression(
      node.operator,
      instrumentConditionsV2(
        node.left,
        frameId,
        conditionTemps,
        nextIndex,
        decisionLogicalNodes,
      ),
      instrumentConditionsV2(
        node.right,
        frameId,
        conditionTemps,
        nextIndex,
        decisionLogicalNodes,
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
      instrumentConditionsV2(
        node.argument,
        frameId,
        conditionTemps,
        nextIndex,
        decisionLogicalNodes,
      ),
      true,
    );
  }

  const index = nextIndex.value;
  nextIndex.value += 1;
  const temporary = conditionTemps[index]!;
  const weight = 3 ** index;
  return t.sequenceExpression([
    t.assignmentExpression("=", t.cloneNode(temporary), node),
    t.assignmentExpression(
      "+=",
      t.cloneNode(frameId),
      t.conditionalExpression(
        t.cloneNode(temporary),
        t.numericLiteral(weight * 2),
        t.numericLiteral(weight),
      ),
    ),
    t.cloneNode(temporary),
  ]);
}

function hitStatement(
  id: string,
  hitRuntimeName: string,
  v2?: {
    fileState: t.Identifier;
    clock: t.Identifier;
    hitEpochs: t.Identifier;
    pointIndex: number;
    epoch?: t.Identifier;
  },
): t.ExpressionStatement {
  if (v2) {
    const fileState = t.cloneNode(v2.fileState);
    const clock = t.cloneNode(v2.clock);
    return t.expressionStatement(
      t.logicalExpression(
        "||",
        t.binaryExpression(
          "===",
          t.memberExpression(
            t.cloneNode(v2.hitEpochs),
            t.numericLiteral(v2.pointIndex),
            true,
          ),
          v2.epoch
            ? t.cloneNode(v2.epoch)
            : t.memberExpression(t.cloneNode(clock), t.identifier("epoch")),
        ),
        t.callExpression(t.identifier(hitRuntimeName), [
          t.cloneNode(fileState),
          t.numericLiteral(v2.pointIndex),
        ]),
      ),
    );
  }
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
    REGISTER_V2,
    END_V2,
    HIT_V2,
    FILE_V2,
    CLOCK_V2,
    HITS_V2,
    DECISIONS_V2,
    COMPLETE_V2,
    SELECTION_BEGIN,
    SELECTION_RIGHT,
    SELECTION_END,
    WITH_REQUEST_PHASE,
    OPTIONAL_SELECT,
    OPTIONAL_CALL_BEGIN,
    OPTIONAL_CALL_REACHED,
    OPTIONAL_CALL_CONTINUED,
    OPTIONAL_CALL_END,
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

function executablePointNode(node: t.Statement): t.Statement | undefined {
  if (t.isExportNamedDeclaration(node) || t.isExportDefaultDeclaration(node)) {
    const declaration = node.declaration;
    return declaration && t.isStatement(declaration) &&
        isExecutableStatement(declaration)
      ? declaration
      : undefined;
  }
  return isExecutableStatement(node) ? node : undefined;
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

function isInsideFunctionParameter(path: NodePath): boolean {
  let current: NodePath | null = path;
  while (current?.parentPath) {
    if (current.parentPath.isFunction()) return current.listKey === "params";
    current = current.parentPath;
  }
  return false;
}

export interface InstrumentMcdcResult {
  code: string;
  map: ReturnType<typeof generate>["map"];
  manifest: CoverageManifest;
  decisions: McdcDecisionMeta[];
}

export interface InstrumentMcdcOptions {
  /** Experimental probe transport; v1 remains the public default. */
  probeVersion?: 1 | 2;
}

/** Number encoding remains exact while both 3^n and its doubled outcome index are safe. */
export const PROBE_V2_MAX_CONDITIONS = 32;
export const PROBE_V2_DENSE_CONDITION_LIMIT = 6;

export function instrumentMcdc(
  code: string,
  file: string,
  options: InstrumentMcdcOptions = {},
): InstrumentMcdcResult {
  const names = allocatedRuntimeNames(code);
  const BEGIN = names["__supercovMcdcBegin"]!;
  const CONDITION = names["__supercovMcdcCondition"]!;
  const END = names["__supercovMcdcEnd"]!;
  const HIT = names["__supercovCoverageHit"]!;
  const REGISTER_V2 = names["__supercovRegisterProbeV2"]!;
  const END_V2 = names["__supercovMcdcEndV2"]!;
  const HIT_V2 = names["__supercovCoverageHitV2"]!;
  const FILE_V2 = names["__supercovProbeFileV2"]!;
  const CLOCK_V2 = names["__supercovProbeClockV2"]!;
  const HITS_V2 = names["__supercovProbeHitsV2"]!;
  const DECISIONS_V2 = names["__supercovProbeDecisionsV2"]!;
  const COMPLETE_V2 = names["__supercovProbeCompleteV2"]!;
  const probeVersion = options.probeVersion ?? 1;
  const SELECTION_BEGIN = names["__supercovSelectionBegin"]!;
  const SELECTION_RIGHT = names["__supercovSelectionRight"]!;
  const SELECTION_END = names["__supercovSelectionEnd"]!;
  const WITH_REQUEST_PHASE = names["__supercovWithRequestPhase"]!;
  const OPTIONAL_SELECT = names["__supercovOptionalSelect"]!;
  const OPTIONAL_CALL_BEGIN = names["__supercovOptionalCallBegin"]!;
  const OPTIONAL_CALL_REACHED = names["__supercovOptionalCallReached"]!;
  const OPTIONAL_CALL_CONTINUED = names["__supercovOptionalCallContinued"]!;
  const OPTIONAL_CALL_END = names["__supercovOptionalCallEnd"]!;
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
  const decisionVectorCounts: number[] = [];
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

  // A synchronous function cannot suspend while its body is executing. A
  // nested coverage scope restores the caller's epoch before returning, so
  // interior probes can compare against one scalar captured at entry instead
  // of repeatedly loading the shared clock object. Async functions and
  // generators deliberately keep the shared clock: either may resume under a
  // later attribution epoch. Parameter initializers also use the shared clock
  // because they execute before this body-local declaration.
  const functionEpochs = new WeakMap<t.Function, t.Identifier>();
  const functionDecisionCaches = new WeakMap<
    t.Function,
    { bits: t.Identifier; next: number }
  >();
  const decisionCacheDeclarations: Array<{
    body: t.BlockStatement;
    bits: t.Identifier;
  }> = [];
  if (probeVersion === 2) {
    traverse(ast, {
      Function(path) {
        const node = path.node;
        if (
          node.async ||
          node.generator ||
          !t.isBlockStatement(node.body) ||
          isUnsafeInstrumentationContext(path)
        )
          return;
        const epoch = path.scope.generateUidIdentifier("supercovEpoch");
        const declaration = t.variableDeclaration("const", [
          t.variableDeclarator(
            t.cloneNode(epoch),
            t.memberExpression(t.identifier(CLOCK_V2), t.identifier("epoch")),
          ),
        ]);
        generatedStatements.add(declaration);
        node.body.body.unshift(declaration);
        functionEpochs.set(node, epoch);
      },
    });
  }
  const functionOwnerForPath = (
    path: NodePath,
  ): NodePath<t.Function> | undefined => {
    const owner = path.findParent((parent) => parent.isFunction());
    return owner?.isFunction() ? owner : undefined;
  };
  const epochForPath = (path: NodePath): t.Identifier | undefined => {
    const owner = functionOwnerForPath(path);
    return owner ? functionEpochs.get(owner.node) : undefined;
  };
  const localDecisionForPath = (
    path: NodePath,
  ): { bits: t.Identifier; mask: number } | undefined => {
    const owner = functionOwnerForPath(path);
    if (
      !owner ||
      !functionEpochs.has(owner.node) ||
      !t.isBlockStatement(owner.node.body)
    )
      return undefined;
    let cache = functionDecisionCaches.get(owner.node);
    if (!cache) {
      cache = {
        bits: owner.scope.generateUidIdentifier("supercovComplete"),
        next: 0,
      };
      functionDecisionCaches.set(owner.node, cache);
      decisionCacheDeclarations.push({
        body: owner.node.body,
        bits: cache.bits,
      });
    }
    const bit = cache.next++;
    return bit < 30 ? { bits: cache.bits, mask: 2 ** bit } : undefined;
  };

  // Record executable statements before adding any instrumentation statements.
  const instrumentedStatements = new WeakSet<t.Statement>();
  traverse(ast, {
    Statement(path) {
      const node = path.node;
      if (instrumentedStatements.has(node)) return;
      if (
        (path.parentPath.isExportNamedDeclaration() ||
          path.parentPath.isExportDefaultDeclaration()) &&
        path.parentPath.node.declaration === node
      )
        return;
      const pointNode = executablePointNode(node);
      if (
        !pointNode?.loc ||
        isUnsafeInstrumentationContext(path) ||
        generatedStatements.has(node) ||
        !isExecutableStatement(pointNode)
      )
        return;
      if (path.parentPath.isLabeledStatement()) return;

      const id = stableId(file, "statement", pointNode);
      points.push({
        id,
        kind: "statement",
        file,
        line: pointNode.loc.start.line,
        column: pointNode.loc.start.column + 1,
        source: sourceFor(code, pointNode),
      });
      const probe = hitStatement(
        id,
        probeVersion === 2 ? HIT_V2 : HIT,
        probeVersion === 2
          ? {
              fileState: t.identifier(FILE_V2),
              clock: t.identifier(CLOCK_V2),
              hitEpochs: t.identifier(HITS_V2),
              pointIndex: points.length - 1,
              epoch: epochForPath(path),
            }
          : undefined,
      );
      generatedStatements.add(probe);
      instrumentedStatements.add(node);

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
  });

  interface OptionalCallSite {
    node: t.OptionalCallExpression;
    frame: t.Identifier;
    shortId: string;
    continuedId: string;
  }
  const optionalCallChains = new Map<
    t.Expression,
    { path: NodePath<t.Expression>; sites: OptionalCallSite[] }
  >();
  traverse(ast, {
    OptionalCallExpression(path) {
      if (
        isUnsafeInstrumentationContext(path) ||
        !path.node.optional ||
        !path.node.loc ||
        !t.isExpression(path.node.callee)
      )
        return;
      const callee = path.node.callee;
      if (
        t.isOptionalMemberExpression(callee) &&
        t.isPrivateName(callee.property)
      ) {
        limitations.push({
          id: stableId(
            file,
            "semantic-safety",
            path.node,
            "optional-private-call",
          ),
          kind: "semantic-safety",
          file,
          line: path.node.loc.start.line,
          column: path.node.loc.start.column + 1,
          source: sourceFor(code, path.node),
          reason:
            "optional private-method calls are left native because private names cannot be wrapped as computed reference keys",
        });
        return;
      }
      let root = path as unknown as NodePath<t.Expression>;
      while (
        (root.parentPath.isOptionalCallExpression() &&
          root.parentPath.node.callee === root.node) ||
        (root.parentPath.isOptionalMemberExpression() &&
          root.parentPath.node.object === root.node)
      )
        root = root.parentPath as unknown as NodePath<t.Expression>;
      if (
        root.parentPath.isUnaryExpression({ operator: "delete" }) &&
        root.parentPath.node.argument === root.node
      )
        root = root.parentPath as unknown as NodePath<t.Expression>;
      const id = stableId(file, "optional-chain", path.node, "call");
      const shortId = `${id}:short`;
      const continuedId = `${id}:continued`;
      branches.push({
        id,
        kind: "optional-chain",
        file,
        line: path.node.loc.start.line,
        column: path.node.loc.start.column + 1,
        source: sourceFor(code, path.node),
        alternatives: [
          { id: shortId, label: "nullish / short-circuited" },
          { id: continuedId, label: "non-nullish / continued" },
        ],
      });
      const chain = optionalCallChains.get(root.node) ?? {
        path: root,
        sites: [],
      };
      chain.sites.push({
        node: path.node,
        frame: root.scope.generateUidIdentifier("optionalCall"),
        shortId,
        continuedId,
      });
      optionalCallChains.set(root.node, chain);
    },
  });
  for (const chain of optionalCallChains.values()) {
    for (const site of chain.sites) {
      chain.path.scope.push({ id: t.cloneNode(site.frame), kind: "let" });
      const callee = site.node.callee;
      if (
        t.isMemberExpression(callee) ||
        t.isOptionalMemberExpression(callee)
      ) {
        if (t.isPrivateName(callee.property)) {
          if (t.isExpression(callee.object))
            callee.object = t.callExpression(
              t.identifier(OPTIONAL_CALL_REACHED),
              [t.cloneNode(site.frame), callee.object],
            );
          site.node.arguments.unshift(
            t.spreadElement(
              t.callExpression(t.identifier(OPTIONAL_CALL_CONTINUED), [
                t.cloneNode(site.frame),
              ]),
            ),
          );
          continue;
        }
        const property = callee.computed
          ? callee.property
          : t.isIdentifier(callee.property)
            ? t.stringLiteral(callee.property.name)
            : callee.property;
        callee.computed = true;
        callee.property = t.callExpression(
          t.identifier(OPTIONAL_CALL_REACHED),
          [t.cloneNode(site.frame), property],
        );
      } else {
        site.node.callee = t.callExpression(
          t.identifier(OPTIONAL_CALL_REACHED),
          [t.cloneNode(site.frame), callee],
        );
      }
      site.node.arguments.unshift(
        t.spreadElement(
          t.callExpression(t.identifier(OPTIONAL_CALL_CONTINUED), [
            t.cloneNode(site.frame),
          ]),
        ),
      );
    }
    let measured = chain.path.node;
    for (const site of [...chain.sites].reverse())
      measured = t.callExpression(t.identifier(OPTIONAL_CALL_END), [
        t.cloneNode(site.frame),
        measured,
      ]);
    chain.path.replaceWith(
      t.sequenceExpression([
        ...chain.sites.map((site) =>
          t.assignmentExpression(
            "=",
            t.cloneNode(site.frame),
            t.callExpression(t.identifier(OPTIONAL_CALL_BEGIN), [
              t.stringLiteral(site.shortId),
              t.stringLiteral(site.continuedId),
            ]),
          ),
        ),
        measured,
      ]),
    );
  }

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
        const frameId = path.scope.generateUidIdentifier(
          "supercovSelectionFrame",
        );
        const frameScope =
          path.scope.getFunctionParent() ?? path.scope.getProgramParent();
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
      if (entries.length === 0) return;
      if (t.isBlockStatement(path.node.body)) {
        path.node.body.body.unshift(...entries);
        return;
      }
      if (!path.isArrowFunctionExpression())
        throw new Error("Only arrow functions may have expression bodies");
      const value = path.node.body;
      path.node.body = t.blockStatement([
        ...entries,
        t.returnStatement(value),
      ]);
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
            reason:
              "destructuring defaults in a classic for initializer cannot yet be finalized without restructuring control flow",
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
  const tryTargets = new WeakMap<
    t.TryStatement,
    { successId: string; catchId: string }
  >();
  const enumerationTargets = new WeakMap<
    t.ForInStatement | t.ForOfStatement,
    { zeroId: string; enteredId: string }
  >();
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
      tryTargets.set(node, { successId, catchId });
    },
    "ForInStatement|ForOfStatement"(
      path: NodePath<t.ForInStatement | t.ForOfStatement>,
    ) {
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
      enumerationTargets.set(node, { zeroId, enteredId });
    },
  });

  // Transform bottom-up after the denominator is frozen. This keeps nested
  // try/loop nodes attached to the tree while they are rewritten; mutating an
  // outer path first can leave a nested Babel path detached while still
  // recording an obligation that can never emit evidence.
  traverse(ast, {
    TryStatement: {
      exit(path) {
        if (isUnsafeInstrumentationContext(path)) return;
        const node = path.node;
        const target = tryTargets.get(node);
        if (!target || !node.handler) return;
        const frameId = path.scope.generateUidIdentifier("supercovTryFrame");
        const frameScope =
          path.scope.getFunctionParent() ?? path.scope.getProgramParent();
        frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
        path.insertBefore(
          t.expressionStatement(
            t.assignmentExpression(
              "=",
              t.cloneNode(frameId),
              t.callExpression(t.identifier(TRY_BEGIN), [
                t.stringLiteral(target.successId),
                t.stringLiteral(target.catchId),
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
    },
    "ForInStatement|ForOfStatement": {
      exit(path: NodePath<t.ForInStatement | t.ForOfStatement>) {
        if (isUnsafeInstrumentationContext(path)) return;
        const node = path.node;
        const target = enumerationTargets.get(node);
        if (!target) return;
        const frameId = path.scope.generateUidIdentifier("supercovLoopFrame");
        const frameScope =
          path.scope.getFunctionParent() ?? path.scope.getProgramParent();
        frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
        const assignment = t.expressionStatement(
          t.assignmentExpression(
            "=",
            t.cloneNode(frameId),
            t.callExpression(t.identifier(LOOP_BEGIN), [
              t.stringLiteral(target.zeroId),
              t.stringLiteral(target.enteredId),
            ]),
          ),
        );
        const loop = t.cloneNode(node, true);
        restoreClonedOffsets(node, loop);
        loop.body = t.isBlockStatement(loop.body)
          ? loop.body
          : t.blockStatement([loop.body]);
        loop.body.body.unshift(
          t.expressionStatement(
            probeVersion === 2
              ? t.assignmentExpression(
                  "=",
                  t.memberExpression(
                    t.cloneNode(frameId),
                    t.identifier("entered"),
                  ),
                  t.booleanLiteral(true),
                )
              : t.callExpression(t.identifier(LOOP_ENTERED), [
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
      const probe = hitStatement(
        id,
        probeVersion === 2 ? HIT_V2 : HIT,
        probeVersion === 2
          ? {
              fileState: t.identifier(FILE_V2),
              clock: t.identifier(CLOCK_V2),
              hitEpochs: t.identifier(HITS_V2),
              pointIndex: points.length - 1,
            }
          : undefined,
      );
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
    const decisionIndex = decisions.length;
    decisions.push(meta);

    // A loop predicate's path scope may be represented by Babel as the loop
    // body's block. Declaring the frame there puts it after the predicate that
    // uses it (and is especially visible in async generators). Hoist scratch
    // frames to the nearest function/program scope instead.
    const inlineFrame = isInsideFunctionParameter(path);
    const evaluationScope =
      (path.isFunctionExpression() ||
        path.isArrowFunctionExpression() ||
        path.isClassExpression()) &&
      path.scope.parent
        ? path.scope.parent
        : path.scope;
    const frameScope =
      evaluationScope.getFunctionParent() ?? evaluationScope.getProgramParent();
    const frameId = frameScope.generateUidIdentifier("supercovMcdcFrame");
    const useV2 =
      probeVersion === 2 &&
      originalConditions.length <= PROBE_V2_MAX_CONDITIONS;
    const conditionTemps = useV2
      ? originalConditions.map(() =>
          frameScope.generateUidIdentifier("supercovMcdcValue"),
        )
      : [];
    const resultTemp = useV2
      ? frameScope.generateUidIdentifier("supercovMcdcResult")
      : undefined;
    const useDenseV2 =
      useV2 && originalConditions.length <= PROBE_V2_DENSE_CONDITION_LIMIT;
    const useSaturatedV2 =
      useDenseV2 && isCoverageTransparentDecision(originalConditions);
    const originalDecision = useSaturatedV2
      ? t.cloneNode(path.node, true)
      : undefined;
    if (originalDecision)
      markDecisionLogicalNodes(originalDecision, decisionLogicalNodes);
    decisionVectorCounts.push(
      useSaturatedV2
        ? probeV2ReachableVectorCount(path.node, originalConditions)
        : 0,
    );
    const decisionEpoch = inlineFrame ? undefined : epochForPath(path);
    const localComplete =
      useSaturatedV2 && !inlineFrame ? localDecisionForPath(path) : undefined;
    const vectorTemp = useDenseV2
      ? frameScope.generateUidIdentifier("supercovMcdcVector")
      : undefined;
    if (!inlineFrame) {
      frameScope.push({ id: t.cloneNode(frameId), kind: "let" });
      for (const temporary of conditionTemps)
        frameScope.push({ id: t.cloneNode(temporary), kind: "let" });
      if (resultTemp)
        frameScope.push({ id: t.cloneNode(resultTemp), kind: "let" });
      if (vectorTemp)
        frameScope.push({ id: t.cloneNode(vectorTemp), kind: "let" });
    }
    const instrumented = useV2
      ? instrumentConditionsV2(
          path.node,
          frameId,
          conditionTemps,
          { value: 0 },
          decisionLogicalNodes,
        )
      : instrumentConditions(
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
      useV2 ? t.numericLiteral(0) : begin,
    );
    const end = useV2
      ? t.callExpression(t.identifier(END_V2), [
          t.identifier(FILE_V2),
          t.numericLiteral(decisionIndex),
          t.cloneNode(frameId),
          t.cloneNode(resultTemp!),
        ])
      : t.callExpression(t.identifier(END), [
          t.cloneNode(frameId),
          instrumented,
        ]);
    const v2Tail: t.Expression[] = useV2
      ? [
          t.assignmentExpression("=", t.cloneNode(resultTemp!), instrumented),
          ...(useDenseV2
            ? [
                t.assignmentExpression(
                  "=",
                  t.cloneNode(vectorTemp!),
                  t.binaryExpression(
                    "+",
                    t.binaryExpression(
                      "*",
                      t.cloneNode(frameId),
                      t.numericLiteral(2),
                    ),
                    t.conditionalExpression(
                      t.cloneNode(resultTemp!),
                      t.numericLiteral(1),
                      t.numericLiteral(0),
                    ),
                  ),
                ),
                t.logicalExpression(
                  "||",
                  t.binaryExpression(
                    "===",
                    t.memberExpression(
                      t.memberExpression(
                        t.identifier(DECISIONS_V2),
                        t.numericLiteral(decisionIndex),
                        true,
                      ),
                      t.cloneNode(vectorTemp!),
                      true,
                    ),
                    decisionEpoch
                      ? t.cloneNode(decisionEpoch)
                      : t.memberExpression(
                          t.identifier(CLOCK_V2),
                          t.identifier("epoch"),
                        ),
                  ),
                  end,
                ),
                t.cloneNode(resultTemp!),
              ]
            : [end]),
        ]
      : [end];
    const observedDecision = inlineFrame
      ? t.callExpression(
          t.arrowFunctionExpression(
            [],
            t.blockStatement([
              t.variableDeclaration("let", [
                t.variableDeclarator(
                  t.cloneNode(frameId),
                  useV2 ? t.numericLiteral(0) : begin,
                ),
                ...conditionTemps.map((temporary) =>
                  t.variableDeclarator(t.cloneNode(temporary)),
                ),
                ...(resultTemp
                  ? [t.variableDeclarator(t.cloneNode(resultTemp))]
                  : []),
                ...(vectorTemp
                  ? [t.variableDeclarator(t.cloneNode(vectorTemp))]
                  : []),
              ]),
              t.returnStatement(useV2 ? t.sequenceExpression(v2Tail) : end),
            ]),
          ),
          [],
        )
      : t.sequenceExpression([assignFrame, ...(useV2 ? v2Tail : [end])]);
    const completeCheck = useSaturatedV2
      ? t.binaryExpression(
          "===",
          t.memberExpression(
            t.identifier(COMPLETE_V2),
            t.numericLiteral(decisionIndex),
            true,
          ),
          decisionEpoch
            ? t.cloneNode(decisionEpoch)
            : t.memberExpression(t.identifier(CLOCK_V2), t.identifier("epoch")),
        )
      : undefined;
    const completeFastPath =
      completeCheck && localComplete
        ? t.logicalExpression(
            "||",
            t.binaryExpression(
              "!==",
              t.binaryExpression(
                "&",
                t.cloneNode(localComplete.bits),
                t.numericLiteral(localComplete.mask),
              ),
              t.numericLiteral(0),
            ),
            t.logicalExpression(
              "&&",
              completeCheck,
              t.assignmentExpression(
                "|=",
                t.cloneNode(localComplete.bits),
                t.numericLiteral(localComplete.mask),
              ),
            ),
          )
        : completeCheck;
    path.replaceWith(
      useSaturatedV2
        ? t.conditionalExpression(
            completeFastPath!,
            originalDecision!,
            observedDecision,
          )
        : observedDecision,
    );
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
        reason:
          "eval-generated source has no stable pre-run coverage denominator",
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
        reason:
          "Function-generated source has no stable pre-run coverage denominator",
      });
    },
  });

  // Route requests carry the Playwright phase as a private test-only header.
  // Wrap Remix request entry points so Node's AsyncLocalStorage can propagate
  // that exact phase through every awaited helper without application edits.
  // This runs after source instrumentation so generated wrappers never become
  // coverage obligations themselves.
  if (
    /^app\/routes\//.test(file) ||
    /(?:^|\/)app\/.*\/route\.[cm]?[jt]sx?$/.test(file)
  ) {
    traverse(ast, {
      ExportNamedDeclaration(path) {
        const declaration = path.node.declaration;
        if (t.isVariableDeclaration(declaration)) {
          for (const declarator of declaration.declarations) {
            if (
              t.isIdentifier(declarator.id) &&
              isRequestHandlerName(file, declarator.id.name) &&
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
          isRequestHandlerName(file, declaration.id.name)
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
            isRequestHandlerName(file, specifier.exported.name),
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
        (t.isMemberExpression(callee) ||
          t.isOptionalMemberExpression(callee)) &&
        !callee.computed &&
        t.isIdentifier(callee.property)
          ? callee.property.name
          : undefined;
      const identifier = t.isIdentifier(callee) ? callee.name : property;
      let callbackIndex = -1;
      if (
        (property === "on" ||
          property === "once" ||
          property === "addListener") &&
        t.isStringLiteral(path.node.arguments[0]) &&
        ["request", "upgrade", "connection"].includes(
          path.node.arguments[0].value,
        )
      ) {
        callbackIndex = 1;
      } else if (identifier === "createServer") {
        for (
          let index = path.node.arguments.length - 1;
          index >= 0;
          index -= 1
        ) {
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

  // Decision cache identifiers are discovered during instrumentation. Insert
  // their declarations only after all source traversals so no live NodePath is
  // shifted while a source statement is being rewritten.
  for (const { body, bits } of decisionCacheDeclarations)
    body.body.unshift(
      t.variableDeclaration("let", [
        t.variableDeclarator(t.cloneNode(bits), t.numericLiteral(0)),
      ]),
    );

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
          ...(probeVersion === 2
            ? [
                t.importSpecifier(
                  t.identifier(REGISTER_V2),
                  t.identifier("registerProbeV2"),
                ),
                t.importSpecifier(
                  t.identifier(END_V2),
                  t.identifier("mcdcEndV2"),
                ),
                t.importSpecifier(
                  t.identifier(HIT_V2),
                  t.identifier("coverageHitV2"),
                ),
              ]
            : []),
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
            t.identifier(OPTIONAL_CALL_BEGIN),
            t.identifier("optionalCallBegin"),
          ),
          t.importSpecifier(
            t.identifier(OPTIONAL_CALL_REACHED),
            t.identifier("optionalCallReached"),
          ),
          t.importSpecifier(
            t.identifier(OPTIONAL_CALL_CONTINUED),
            t.identifier("optionalCallContinued"),
          ),
          t.importSpecifier(
            t.identifier(OPTIONAL_CALL_END),
            t.identifier("optionalCallEnd"),
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
      ...(probeVersion === 2
        ? [
            t.variableDeclaration("const", [
              t.variableDeclarator(
                t.identifier(FILE_V2),
                t.callExpression(t.identifier(REGISTER_V2), [
                  t.valueToNode({
                    decisions,
                    pointIds: points.map((point) => point.id),
                    decisionVectorCounts,
                  }),
                ]),
              ),
              t.variableDeclarator(
                t.identifier(CLOCK_V2),
                t.memberExpression(
                  t.identifier(FILE_V2),
                  t.identifier("clock"),
                ),
              ),
              t.variableDeclarator(
                t.identifier(HITS_V2),
                t.memberExpression(
                  t.identifier(FILE_V2),
                  t.identifier("hitEpochs"),
                ),
              ),
              t.variableDeclarator(
                t.identifier(DECISIONS_V2),
                t.memberExpression(
                  t.identifier(FILE_V2),
                  t.identifier("decisionEpochs"),
                ),
              ),
              t.variableDeclarator(
                t.identifier(COMPLETE_V2),
                t.memberExpression(
                  t.identifier(FILE_V2),
                  t.identifier("decisionCompleteEpochs"),
                ),
              ),
            ]),
          ]
        : []),
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
