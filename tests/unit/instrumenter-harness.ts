import { instrumentMcdc } from "../../src/instrumenter";
import type {
  CoverageManifest,
  McdcDecisionMeta,
  McdcVector,
} from "../../src/types";

interface DecisionFrame {
  values: Array<boolean | null>;
}

interface SelectionFrame {
  shortId: string;
  rightId: string;
  rightEvaluated: boolean;
}

interface TryFrame {
  successId: string;
  catchId: string;
  caught: boolean;
}

interface LoopFrame {
  zeroId: string;
  enteredId: string;
  entered: boolean;
}

export interface ProbeEvidence {
  manifest: CoverageManifest;
  vectors: McdcVector[];
  hits: string[];
}

export interface ProgramOutcome {
  status: "returned" | "threw";
  value?: unknown;
  error?: {
    name: string;
    message: string;
    cause?: unknown;
  };
  effects: unknown;
}

interface ProgramExports {
  run: () => unknown;
  observe?: () => unknown;
}

function normalized(value: unknown, seen = new WeakSet<object>()): unknown {
  if (value === undefined) return { $type: "undefined" };
  if (typeof value === "number") {
    if (Number.isNaN(value)) return { $type: "number", value: "NaN" };
    if (Object.is(value, -0)) return { $type: "number", value: "-0" };
    if (value === Infinity) return { $type: "number", value: "Infinity" };
    if (value === -Infinity)
      return { $type: "number", value: "-Infinity" };
    return value;
  }
  if (typeof value === "bigint")
    return { $type: "bigint", value: value.toString() };
  if (typeof value === "symbol")
    return { $type: "symbol", value: value.description };
  if (typeof value === "function")
    return { $type: "function", name: value.name, length: value.length };
  if (value === null || typeof value !== "object") return value;
  if (seen.has(value)) return { $type: "circular" };
  seen.add(value);
  if (Array.isArray(value))
    return value.map((item) => normalized(item, seen));
  if (value instanceof Date)
    return { $type: "date", value: value.toISOString() };
  if (value instanceof Map)
    return {
      $type: "map",
      value: [...value].map(([key, item]) => [
        normalized(key, seen),
        normalized(item, seen),
      ]),
    };
  if (value instanceof Set)
    return { $type: "set", value: [...value].map((item) => normalized(item, seen)) };
  const prototype = Object.getPrototypeOf(value) as
    | { constructor?: { name?: string } }
    | null;
  return {
    ...(prototype?.constructor?.name && prototype.constructor.name !== "Object"
      ? { $prototype: prototype.constructor.name }
      : {}),
    ...Object.fromEntries(
      Reflect.ownKeys(value).map((key) => [
        typeof key === "symbol" ? `[${String(key)}]` : key,
        normalized(Reflect.get(value, key), seen),
      ]),
    ),
  };
}

function runtimeBindings(
  code: string,
  evidence: ProbeEvidence,
): { names: string[]; values: unknown[] } {
  const pendingDefaults = new Map<string, number>();
  const begin = (_id: string, meta: McdcDecisionMeta): DecisionFrame => ({
    values: Array.from({ length: meta.conditions.length }, () => null),
  });
  const condition = <T>(frame: DecisionFrame, index: number, value: T): T => {
    frame.values[index] = Boolean(value);
    return value;
  };
  const end = <T>(frame: DecisionFrame, value: T): T => {
    evidence.vectors.push({ values: [...frame.values], outcome: Boolean(value) });
    return value;
  };
  const selectionBegin = (
    shortId: string,
    rightId: string,
  ): SelectionFrame => ({ shortId, rightId, rightEvaluated: false });
  const applyInferredName = <T>(value: T, inferredName?: string): T => {
    if (inferredName && typeof value === "function" && value.name === "")
      Object.defineProperty(value, "name", {
        value: inferredName,
        configurable: true,
      });
    return value;
  };
  const selectionRight = <T>(
    frame: SelectionFrame,
    value: T,
    inferredName?: string,
  ): T => {
    frame.rightEvaluated = true;
    return applyInferredName(value, inferredName);
  };
  const selectionEnd = <T>(frame: SelectionFrame, value: T): T => {
    evidence.hits.push(frame.rightEvaluated ? frame.rightId : frame.shortId);
    return value;
  };
  const optionalSelect = <T>(
    shortId: string,
    continuedId: string,
    value: T,
  ): T => {
    evidence.hits.push(
      value === null || value === undefined ? shortId : continuedId,
    );
    return value;
  };
  const defaultSelected = <T>(
    id: string,
    value: T,
    inferredName?: string,
  ): T => {
    pendingDefaults.set(id, (pendingDefaults.get(id) ?? 0) + 1);
    return applyInferredName(value, inferredName);
  };
  const defaultEntered = (defaultId: string, providedId: string): void => {
    const pending = pendingDefaults.get(defaultId) ?? 0;
    evidence.hits.push(pending > 0 ? defaultId : providedId);
    if (pending > 0) pendingDefaults.set(defaultId, pending - 1);
  };
  const tryBegin = (successId: string, catchId: string): TryFrame => ({
    successId,
    catchId,
    caught: false,
  });
  const tryCatch = <T>(frame: TryFrame, value: T): T => {
    frame.caught = true;
    return value;
  };
  const tryEnd = (frame: TryFrame): void => {
    evidence.hits.push(frame.caught ? frame.catchId : frame.successId);
  };
  const loopBegin = (zeroId: string, enteredId: string): LoopFrame => ({
    zeroId,
    enteredId,
    entered: false,
  });
  const loopEntered = (frame: LoopFrame): void => {
    frame.entered = true;
  };
  const loopEnd = (frame: LoopFrame): void => {
    evidence.hits.push(frame.entered ? frame.enteredId : frame.zeroId);
  };
  const implementations: Record<string, unknown> = {
    mcdcBegin: begin,
    mcdcCondition: condition,
    mcdcEnd: end,
    coverageHit: (id: string) => evidence.hits.push(id),
    selectionBegin,
    selectionRight,
    selectionEnd,
    optionalSelect,
    defaultSelected,
    defaultEntered,
    tryBegin,
    tryCatch,
    tryEnd,
    loopBegin,
    loopEntered,
    loopEnd,
    withRequestPhase: <T>(handler: T): T => handler,
  };
  const importMatch = code.match(
    /^import\s*\{([\s\S]*?)\}\s*from\s*["']virtual:supercov-runtime["'];?/,
  );
  if (!importMatch) throw new Error("Instrumented code is missing its runtime import");
  const bindings = [...importMatch[1]!.matchAll(/([\w$]+)\s+as\s+([\w$]+)/g)].map(
    ([, imported, local]) => {
      if (!(imported! in implementations))
        throw new Error(`Unknown Supercov runtime import: ${imported}`);
      return [local!, implementations[imported!]] as const;
    },
  );
  return {
    names: bindings.map(([name]) => name),
    values: bindings.map(([, value]) => value),
  };
}

function stripRuntimeImport(code: string): string {
  return code.replace(/^import[\s\S]*?from\s+["']virtual:supercov-runtime["'];?\s*/, "");
}

function compile(
  source: string,
  instrumented: boolean,
  file: string,
): { program: ProgramExports; evidence: ProbeEvidence } {
  const transformed = instrumentMcdc(source, file);
  const evidence: ProbeEvidence = {
    manifest: transformed.manifest,
    vectors: [],
    hits: [],
  };
  const bindings = instrumented
    ? runtimeBindings(transformed.code, evidence)
    : { names: [], values: [] };
  const executable = instrumented
    ? stripRuntimeImport(transformed.code)
    : source;
  // This deliberately evaluates self-contained conformance fixtures in a new
  // lexical scope; it never evaluates repository or user-provided input.
  // eslint-disable-next-line no-new-func
  const factory = new Function(
    ...bindings.names,
    `"use strict";\n${executable}\nreturn { run, observe: typeof observe === "function" ? observe : undefined };`,
  );
  const program = factory(...bindings.values) as ProgramExports;
  if (typeof program.run !== "function")
    throw new Error("Differential fixture must declare function run()");
  return { program, evidence };
}

async function execute(
  source: string,
  instrumented: boolean,
  file: string,
): Promise<{ outcome: ProgramOutcome; evidence: ProbeEvidence }> {
  const { program, evidence } = compile(source, instrumented, file);
  try {
    const value = await program.run();
    return {
      outcome: {
        status: "returned",
        value: normalized(value),
        effects: normalized(program.observe?.() ?? []),
      },
      evidence,
    };
  } catch (error) {
    const failure = error as { name?: unknown; message?: unknown; cause?: unknown };
    const canHaveProperties =
      (typeof failure === "object" && failure !== null) ||
      typeof failure === "function";
    return {
      outcome: {
        status: "threw",
        error: {
          name: String(failure?.name ?? typeof error),
          message: String(failure?.message ?? error),
          ...(canHaveProperties && "cause" in failure
            ? { cause: normalized(failure.cause) }
            : {}),
        },
        effects: normalized(program.observe?.() ?? []),
      },
      evidence,
    };
  }
}

export async function executeDifferential(
  source: string,
  file = "app/differential.ts",
): Promise<{
  original: ProgramOutcome;
  instrumented: ProgramOutcome;
  evidence: ProbeEvidence;
}> {
  // Keep the two executions sequential so timers, microtasks, and any host
  // resources used by a fixture cannot influence the comparison by racing.
  const original = await execute(source, false, file);
  const instrumented = await execute(source, true, file);
  return {
    original: original.outcome,
    instrumented: instrumented.outcome,
    evidence: instrumented.evidence,
  };
}
