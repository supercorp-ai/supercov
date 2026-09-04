import { withNodeAssertionPhase } from "./runtime.mjs";
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
function assertionSource() {
    const lines = new Error().stack?.split("\n").slice(2) ?? [];
    const entry = lines.find((line) => !/(?:nodeAssert|nodeAssertion|runtime)\.[cm]?[jt]s/.test(line) &&
        !line.includes("node:internal"));
    if (!entry)
        return undefined;
    return entry.trim().replace(/^at\s+/, "");
}
function wrapAssertion(original, operation) {
    return new Proxy(original, {
        apply(target, thisArgument, argumentsList) {
            return withNodeAssertionPhase(operation, assertionSource(), () => Reflect.apply(target, thisArgument, argumentsList));
        },
    });
}
function wrapAssertInstance(instance, operation) {
    const cache = new Map();
    return new Proxy(instance, {
        get(target, property, receiver) {
            const value = Reflect.get(target, property, receiver);
            if (typeof value !== "function" || !ASSERTION_METHODS.has(String(property)))
                return value;
            const existing = cache.get(property);
            if (existing)
                return existing;
            const wrapped = wrapAssertion(value.bind(target), `${operation}.${String(property)}`);
            cache.set(property, wrapped);
            return wrapped;
        },
    });
}
function wrappedAssertConstructor(original, operation) {
    return new Proxy(original, {
        construct(target, argumentsList, newTarget) {
            return wrapAssertInstance(Reflect.construct(target, argumentsList, newTarget), operation);
        },
    });
}
export function createNodeAssertAdapter(native, moduleName) {
    const adapter = wrapAssertion(native, `${moduleName}.ok`);
    const nativeRecord = native;
    for (const property of Reflect.ownKeys(nativeRecord)) {
        if (["name", "length", "prototype", "arguments", "caller"].includes(String(property)))
            continue;
        const descriptor = Reflect.getOwnPropertyDescriptor(nativeRecord, property);
        if (!descriptor)
            continue;
        let value = descriptor.value;
        if (typeof value === "function" && ASSERTION_METHODS.has(String(property)))
            value = wrapAssertion(value, `${moduleName}.${String(property)}`);
        else if (property === "Assert" && typeof value === "function")
            value = wrappedAssertConstructor(value, `${moduleName}.Assert`);
        try {
            Reflect.defineProperty(adapter, property, {
                ...descriptor,
                value,
            });
        }
        catch {
            // Exotic builtin descriptors are not required for assertion dispatch.
        }
    }
    return adapter;
}
export function setStrictAssertAdapter(adapter, strict) {
    Reflect.defineProperty(adapter, "strict", {
        configurable: true,
        enumerable: true,
        value: strict,
        writable: true,
    });
}
