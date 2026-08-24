import { withNodeAssertionPhase } from "./runtime.ts";

type AnyFunction = (...args: any[]) => any;
type AssertModule = AnyFunction;

const ASSERTION_METHODS = new Set([
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

function assertionSource(): string | undefined {
  const lines = new Error().stack?.split("\n").slice(2) ?? [];
  const entry = lines.find(
    (line) =>
      !/(?:nodeAssert|nodeAssertion|runtime)\.[cm]?[jt]s/.test(line) &&
      !line.includes("node:internal"),
  );
  if (!entry) return undefined;
  return entry.trim().replace(/^at\s+/, "");
}

function wrapAssertion<T extends AnyFunction>(
  original: T,
  operation: string,
): T {
  return new Proxy(original, {
    apply(target, thisArgument, argumentsList) {
      return withNodeAssertionPhase(operation, assertionSource(), () =>
        Reflect.apply(target, thisArgument, argumentsList),
      );
    },
  }) as T;
}

function wrapAssertInstance(instance: object, operation: string): object {
  const cache = new Map<PropertyKey, unknown>();
  return new Proxy(instance, {
    get(target, property, receiver) {
      const value = Reflect.get(target, property, receiver) as unknown;
      if (typeof value !== "function" || !ASSERTION_METHODS.has(String(property)))
        return value;
      const existing = cache.get(property);
      if (existing) return existing;
      const wrapped = wrapAssertion(
        value.bind(target) as AnyFunction,
        `${operation}.${String(property)}`,
      );
      cache.set(property, wrapped);
      return wrapped;
    },
  });
}

function wrappedAssertConstructor(
  original: new (...args: any[]) => object,
  operation: string,
): new (...args: any[]) => object {
  return new Proxy(original, {
    construct(target, argumentsList, newTarget) {
      return wrapAssertInstance(
        Reflect.construct(target, argumentsList, newTarget),
        operation,
      );
    },
  });
}

export function createNodeAssertAdapter<T extends AssertModule>(
  native: T,
  moduleName: string,
): T {
  const adapter = wrapAssertion(native, `${moduleName}.ok`) as AssertModule;
  const nativeRecord = native as unknown as Record<PropertyKey, unknown>;
  for (const property of Reflect.ownKeys(nativeRecord)) {
    if (["name", "length", "prototype", "arguments", "caller"].includes(String(property)))
      continue;
    const descriptor = Reflect.getOwnPropertyDescriptor(nativeRecord, property);
    if (!descriptor) continue;
    let value = descriptor.value;
    if (typeof value === "function" && ASSERTION_METHODS.has(String(property)))
      value = wrapAssertion(value as AnyFunction, `${moduleName}.${String(property)}`);
    else if (property === "Assert" && typeof value === "function")
      value = wrappedAssertConstructor(
        value as new (...args: any[]) => object,
        `${moduleName}.Assert`,
      );
    try {
      Reflect.defineProperty(adapter, property, {
        ...descriptor,
        value,
      });
    } catch {
      // Exotic builtin descriptors are not required for assertion dispatch.
    }
  }
  return adapter as T;
}

export function setStrictAssertAdapter(
  adapter: AnyFunction,
  strict: AnyFunction,
): void {
  Reflect.defineProperty(adapter, "strict", {
    configurable: true,
    enumerable: true,
    value: strict,
    writable: true,
  });
}
