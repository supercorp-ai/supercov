import * as native from "node:test";
import type { TestContext, TestOptions } from "node:test";
import {
  beginBufferedServerEvidence,
  flushBufferedServerEvidence,
  takeNodeAssertionPhases,
  withCoverageCarrier,
} from "./runtime.ts";
import {
  callerLocation,
  runnerExecutionScope,
  writeRunnerEvidence,
  type RunnerTestIdentity,
} from "./runnerEvidence.ts";

type TestCallback = (context: TestContext, done?: (error?: Error) => void) => unknown;
type TestRegistration = (...args: unknown[]) => unknown;

function callbackIndex(args: unknown[]): number {
  for (let index = args.length - 1; index >= 0; index -= 1)
    if (typeof args[index] === "function") return index;
  return -1;
}

function testName(args: unknown[], callback: TestCallback): string {
  return typeof args[0] === "string" ? args[0] : callback.name || "anonymous test";
}

function testOptions(args: unknown[], index: number): TestOptions | undefined {
  const candidate = args.slice(0, index).find(
    (value) => value && typeof value === "object" && !Array.isArray(value),
  );
  return candidate as TestOptions | undefined;
}

function wrappedRegistration(original: TestRegistration): TestRegistration {
  const wrapped = function supercovNodeTest(this: unknown, ...args: unknown[]): unknown {
    const index = callbackIndex(args);
    if (index < 0) return Reflect.apply(original, this, args);
    const callback = args[index] as TestCallback;
    const location = callerLocation(/(?:nodeTest|runnerEvidence)\.[cm]?[jt]s/);
    const identity: RunnerTestIdentity = {
      runner: "node:test",
      name: testName(args, callback),
      ...location,
    };
    const scope = runnerExecutionScope(identity);
    const options = testOptions(args, index);
    const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
    const next = [...args];
    const execute = (
      callbackThis: unknown,
      context: TestContext,
      done?: (error?: Error) => void,
    ): unknown => {
      // A test or its hooks may intentionally modify Supercov's public
      // environment while testing integrations. Keep this attempt's transport
      // destination fixed to the value present when the test was registered.
      beginBufferedServerEvidence(scope);
      let status: "passed" | "failed" | "skipped" = options?.skip || options?.todo
        ? "skipped"
        : "passed";
      const contextProxy = new Proxy(context, {
        get(target, property, receiver) {
          const value = Reflect.get(target, property, receiver) as unknown;
          if ((property === "skip" || property === "todo") && typeof value === "function") {
            return (...callArgs: unknown[]) => {
              status = "skipped";
              return Reflect.apply(value, target, callArgs);
            };
          }
          if (property === "test" && typeof value === "function")
            return wrappedRegistration(value.bind(target) as TestRegistration);
          return typeof value === "function" ? value.bind(target) : value;
        },
      });
      let emitted = false;
      const emit = (nextStatus = status): void => {
        if (emitted) return;
        emitted = true;
        flushBufferedServerEvidence(scope);
        writeRunnerEvidence(
          identity,
          nextStatus,
          scope,
          evidenceDirectory,
          takeNodeAssertionPhases(scope),
        );
      };
      try {
        if (callback.length >= 2) {
          const callbackDone = (error?: Error): void => {
            if (error) status = "failed";
            emit();
            done?.(error);
          };
          return withCoverageCarrier({ version: 1, scope }, () =>
            Reflect.apply(callback, callbackThis, [contextProxy, callbackDone]),
          );
        }
        const result = withCoverageCarrier({ version: 1, scope }, () =>
          Reflect.apply(callback, callbackThis, [contextProxy]),
        );
        if (result && typeof (result as PromiseLike<unknown>).then === "function")
          return Promise.resolve(result).then(
            (value) => {
              emit();
              return value;
            },
            (error: unknown) => {
              status = "failed";
              emit();
              throw error;
            },
          );
        emit();
        return result;
      } catch (error) {
        status = "failed";
        emit();
        throw error;
      }
    };
    // node:test uses callback arity to distinguish promise/synchronous tests
    // from the legacy done-callback form. Preserve it exactly.
    next[index] = callback.length >= 2
      ? function supercovNodeTestDoneCallback(
          this: unknown,
          context: TestContext,
          done: (error?: Error) => void,
        ): unknown {
          return execute(this, context, done);
        }
      : function supercovNodeTestCallback(
          this: unknown,
          context: TestContext,
        ): unknown {
          return execute(this, context);
        };
    return Reflect.apply(original, this, next);
  } as TestRegistration;
  for (const property of ["skip", "todo", "only"]) {
    const member = (original as unknown as Record<string, unknown>)[property];
    if (typeof member === "function")
      Object.defineProperty(wrapped, property, {
        configurable: true,
        enumerable: true,
        value: wrappedRegistration(member as TestRegistration),
      });
  }
  return wrapped;
}

export const test = wrappedRegistration(native.test as unknown as TestRegistration) as typeof native.test;
export const it = wrappedRegistration(native.it as unknown as TestRegistration) as typeof native.it;
export const suite = native.suite;
export const describe = native.describe;
export const before = native.before;
export const after = native.after;
export const beforeEach = native.beforeEach;
export const afterEach = native.afterEach;
export const mock = native.mock;
export const snapshot = native.snapshot;
export const run = native.run;
export const assert = native.assert;
export default test;
