import childProcess from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync } from "node:fs";
import Module, { syncBuiltinESMExports } from "node:module";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export interface WorkspaceMapping {
  hostRoot: string;
  guestRoot: string;
}

type UnknownRecord = Record<PropertyKey, unknown>;

const HOST_PATH_KEYS = ["hostPath", "source", "src", "localPath"];
const GUEST_PATH_KEYS = [
  "guestPath",
  "target",
  "destination",
  "containerPath",
];
const CACHE_IDENTITY = /^(?:warmup|snapshot|cache)(?:key|tag|id)$/i;
const patchedBuilders = new WeakSet<Function>();
const exportedValues = new WeakSet<object>();
const capabilityProxies = new WeakMap<object, object>();
const importedCapabilityProxies = new WeakMap<object, object>();
let installed = false;
let remoteLaunchSequence = 0;

function executionLogPath(path: string): string {
  const shard = (process.env["SUPERCOV_EXECUTION_LOG_SHARD"] ?? "host").replace(
    /[^A-Za-z0-9_.-]/g,
    "_",
  );
  const suffix = `.${shard}.${process.pid}.jsonl`;
  return path.endsWith(".jsonl")
    ? `${path.slice(0, -".jsonl".length)}${suffix}`
    : `${path}${suffix}`;
}

function record(value: Record<string, unknown>): void {
  const configuredPath = process.env["SUPERCOV_EXECUTION_LOG"];
  if (!configuredPath) return;
  const path = executionLogPath(configuredPath);
  try {
    mkdirSync(dirname(path), { recursive: true });
    appendFileSync(
      path,
      `${JSON.stringify({
        at: new Date().toISOString(),
        pid: process.pid,
        ppid: process.ppid,
        ...value,
      })}\n`,
    );
  } catch {
    // Process tracing is diagnostic and must never change test behavior.
  }
}

function safeArgument(value: string): string | { bytes: number; sha256: string } {
  if (value.length <= 160 && !value.includes("\n") && !value.includes("\r"))
    return value;
  return {
    bytes: Buffer.byteLength(value),
    sha256: createHash("sha256").update(value).digest("hex"),
  };
}

function commandSummary(argv: string[]): Record<string, unknown> {
  return {
    executable: argv[0] ? safeArgument(argv[0]) : undefined,
    arguments: argv.slice(1, 9).map(safeArgument),
    argumentCount: Math.max(0, argv.length - 1),
  };
}

function stringProperty(
  value: UnknownRecord,
  keys: readonly string[],
): string | undefined {
  for (const key of keys) {
    const candidate = value[key];
    if (typeof candidate === "string" && candidate.length > 0)
      return candidate;
  }
  return undefined;
}

function containsPath(parent: string, child: string): string | undefined {
  const local = relative(resolve(parent), resolve(child));
  if (local === "") return "";
  if (local.startsWith("..") || isAbsolute(local)) return undefined;
  return local;
}

/**
 * Find a host-to-guest mount that makes the isolated project visible to a
 * remote executor. This deliberately recognizes data shape, not a provider or
 * package name.
 */
export function discoverWorkspaceMapping(
  value: unknown,
  hostRoot: string,
  depth = 0,
): WorkspaceMapping | undefined {
  if (!value || typeof value !== "object" || depth > 5) return undefined;
  if (Array.isArray(value)) {
    for (const item of value.slice(0, 200)) {
      const mapping = discoverWorkspaceMapping(item, hostRoot, depth + 1);
      if (mapping) return mapping;
    }
    return undefined;
  }
  const candidate = value as UnknownRecord;
  const hostPath = stringProperty(candidate, HOST_PATH_KEYS);
  const guestPath = stringProperty(candidate, GUEST_PATH_KEYS);
  if (hostPath && guestPath) {
    const local = containsPath(hostPath, hostRoot);
    if (local !== undefined) {
      return {
        hostRoot: resolve(hostRoot),
        guestRoot: local ? resolve(guestPath, local) : resolve(guestPath),
      };
    }
  }
  for (const nested of Object.values(candidate).slice(0, 200)) {
    const mapping = discoverWorkspaceMapping(nested, hostRoot, depth + 1);
    if (mapping) return mapping;
  }
  return undefined;
}

function scopeCacheValue(value: string, fingerprint: string): string {
  const suffix = `supercov-${fingerprint.slice(0, 20)}`;
  return value.includes(suffix) ? value : `${value}-${suffix}`;
}

/** Clone only branches that contain a recognized cache/snapshot identity. */
export function scopeCapabilityCache(
  value: unknown,
  fingerprint: string,
  depth = 0,
): { value: unknown; changed: string[] } {
  if (!value || typeof value !== "object" || depth > 5)
    return { value, changed: [] };
  if (Array.isArray(value)) {
    const changed: string[] = [];
    let cloned: unknown[] | undefined;
    for (let index = 0; index < value.length; index += 1) {
      const nested = scopeCapabilityCache(value[index], fingerprint, depth + 1);
      if (nested.changed.length > 0) {
        cloned ??= [...value];
        cloned[index] = nested.value;
        changed.push(...nested.changed.map((path) => `[${index}]${path}`));
      }
    }
    return { value: cloned ?? value, changed };
  }
  const source = value as UnknownRecord;
  let clone: UnknownRecord | undefined;
  const changed: string[] = [];
  for (const key of Reflect.ownKeys(source)) {
    if (typeof key !== "string") continue;
    const nestedValue = source[key];
    if (CACHE_IDENTITY.test(key) && typeof nestedValue === "string") {
      clone ??= { ...source };
      clone[key] = scopeCacheValue(nestedValue, fingerprint);
      changed.push(key);
      continue;
    }
    const nested = scopeCapabilityCache(nestedValue, fingerprint, depth + 1);
    if (nested.changed.length > 0) {
      clone ??= { ...source };
      clone[key] = nested.value;
      changed.push(...nested.changed.map((path) => `${key}.${path}`));
    }
  }
  return { value: clone ?? value, changed };
}

function replaceRoot(value: string, mapping: WorkspaceMapping): string {
  const hostRoot = mapping.hostRoot.replaceAll("\\", "/").replace(/\/$/, "");
  const guestRoot = mapping.guestRoot.replaceAll("\\", "/").replace(/\/$/, "");
  const normalized = value.replaceAll("\\", "/");
  const hostUrl = pathToFileURL(mapping.hostRoot).href.replace(/\/$/, "");
  const guestUrl = pathToFileURL(mapping.guestRoot).href.replace(/\/$/, "");
  if (normalized === hostRoot) return guestRoot;
  if (normalized.startsWith(`${hostRoot}/`))
    return `${guestRoot}/${normalized.slice(hostRoot.length + 1)}`;
  if (value === hostUrl) return guestUrl;
  if (value.startsWith(`${hostUrl}/`))
    return `${guestUrl}/${value.slice(hostUrl.length + 1)}`;
  return value;
}

function appendNodeImport(
  existing: string | undefined,
  registerUrl: string,
): string {
  const addition = `--import=${registerUrl}`;
  if (existing?.includes(addition)) return existing;
  return [existing, addition].filter(Boolean).join(" ");
}

function coverageVariables(environment: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(environment).filter(([key, value]) =>
      key.startsWith("SUPERCOV_") && value !== undefined,
    ),
  );
}

/** Build an environment whose paths remain valid inside the discovered VM. */
export function guestCoverageEnvironment(
  mapping: WorkspaceMapping,
  coverageEnvironment: NodeJS.ProcessEnv = process.env,
  existingEnvironment: NodeJS.ProcessEnv = {},
): NodeJS.ProcessEnv {
  const translated = Object.fromEntries(
    Object.entries(coverageVariables(coverageEnvironment)).map(([key, value]) => [
      key,
      value === undefined ? value : replaceRoot(value, mapping),
    ]),
  );
  const registerUrl = pathToFileURL(
    resolve(mapping.guestRoot, ".supercov/register.mjs"),
  ).href;
  return {
    ...existingEnvironment,
    ...translated,
    SUPERCOV_PROJECT_ROOT: mapping.guestRoot,
    SUPERCOV_CJS_INTERCEPT: "1",
    NODE_OPTIONS: appendNodeImport(existingEnvironment.NODE_OPTIONS, registerUrl),
  };
}

function launchOptions(value: unknown): value is UnknownRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const candidate = value as UnknownRecord;
  return (
    (Array.isArray(candidate.argv) &&
      candidate.argv.every((item) => typeof item === "string")) ||
    (Array.isArray(candidate.cmd) &&
      candidate.cmd.every((item) => typeof item === "string")) ||
    typeof candidate.command === "string"
  );
}

function launchArgv(value: UnknownRecord): string[] {
  if (Array.isArray(value.argv)) return value.argv as string[];
  if (Array.isArray(value.cmd)) return value.cmd as string[];
  return [String(value.command)];
}

function injectRemoteLaunch(
  options: UnknownRecord,
  mapping: WorkspaceMapping,
): UnknownRecord {
  const environmentKey =
    "environment" in options && !("env" in options) ? "environment" : "env";
  const existing = options[environmentKey];
  return {
    ...options,
    [environmentKey]: guestCoverageEnvironment(
      mapping,
      {
        ...process.env,
        SUPERCOV_EXECUTION_LOG_SHARD: `${process.pid}-${++remoteLaunchSequence}`,
      },
      existing && typeof existing === "object"
        ? (existing as NodeJS.ProcessEnv)
        : {},
    ),
  };
}

function wrapResult(value: unknown, mapping: WorkspaceMapping): unknown {
  if (value instanceof Promise)
    return value.then((result) => wrapCapabilityObject(result, mapping));
  return wrapCapabilityObject(value, mapping);
}

/**
 * A remote SDK can hide its first executable launch inside a configuration
 * callback (for example an image warmup hook). Decorate callbacks in ordinary
 * configuration data so capability objects delivered later receive the same
 * provider-neutral launch supervision as objects returned directly by the SDK.
 * Accessors and class instances are deliberately left alone.
 */
export function wrapCapabilityCallbacks(
  value: unknown,
  mapping: WorkspaceMapping,
  depth = 0,
  seen = new WeakMap<object, unknown>(),
): unknown {
  if (typeof value === "function") {
    const cached = seen.get(value);
    if (cached) return cached;
    const original = value;
    const wrapped = function supercovCapabilityCallback(
      this: unknown,
      ...args: unknown[]
    ) {
      return wrapResult(
        Reflect.apply(
          original,
          this,
          args.map((argument) => wrapCapabilityObject(argument, mapping)),
        ),
        mapping,
      );
    };
    seen.set(value, wrapped);
    return wrapped;
  }
  if (!value || typeof value !== "object" || depth > 5) return value;
  const cached = seen.get(value);
  if (cached) return cached;
  const prototype = Object.getPrototypeOf(value);
  if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null)
    return value;

  const clone: unknown[] | UnknownRecord = Array.isArray(value)
    ? [...value]
    : Object.create(prototype);
  seen.set(value, clone);
  let changed = false;
  for (const key of Reflect.ownKeys(value).slice(0, 200)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor || !("value" in descriptor)) continue;
    const wrapped = wrapCapabilityCallbacks(
      descriptor.value,
      mapping,
      depth + 1,
      seen,
    );
    if (wrapped !== descriptor.value) changed = true;
    Object.defineProperty(clone, key, { ...descriptor, value: wrapped });
  }
  if (!changed) {
    seen.set(value, value);
    return value;
  }
  return clone;
}

/**
 * Follow an opaque image/pool/machine-style object graph. Any method accepting
 * an argv-shaped options object receives the translated Supercov environment.
 */
export function wrapCapabilityObject(
  value: unknown,
  mapping: WorkspaceMapping,
): unknown {
  if ((!value || typeof value !== "object") && typeof value !== "function")
    return value;
  const object = value as object;
  const cached = capabilityProxies.get(object);
  if (cached) return cached;
  const proxy = new Proxy(object, {
    get(target, property) {
      // Use the real target as the receiver so SDK getters backed by private
      // fields keep their brand check. Method calls are likewise bound below.
      const member = Reflect.get(target, property, target) as unknown;
      if (typeof member !== "function") return member;
      return (...args: unknown[]) => {
        let callArguments = args;
        let index = args.findIndex(launchOptions);
        let positionalCommand: string[] | undefined;
        if (
          index < 0 &&
          typeof property === "string" &&
          /^(?:exec|execute|launch|run|spawn)$/i.test(property)
        ) {
          const commandIndex = args.findIndex(
            (argument) =>
              typeof argument === "string" ||
              (Array.isArray(argument) && argument.every((item) => typeof item === "string")),
          );
          if (commandIndex >= 0) {
            const command = args[commandIndex];
            positionalCommand = Array.isArray(command)
              ? (command as string[])
              : [String(command), ...(Array.isArray(args[commandIndex + 1]) ? args[commandIndex + 1] as string[] : [])];
            index = args.findIndex(
              (argument, argumentIndex) =>
                argumentIndex > commandIndex &&
                Boolean(argument) &&
                typeof argument === "object" &&
                !Array.isArray(argument),
            );
          }
        }
        if (index >= 0) {
          const original = args[index] as UnknownRecord;
          callArguments = [...args];
          callArguments[index] = injectRemoteLaunch(original, mapping);
          record({
            event: "remote-launch",
            command: commandSummary(positionalCommand ?? launchArgv(original)),
            guestRoot: mapping.guestRoot,
          });
        }
        return wrapResult(
          Reflect.apply(member, target, callArguments),
          mapping,
        );
      };
    },
  });
  capabilityProxies.set(object, proxy);
  return proxy;
}

/**
 * Proxy a value imported by a first-party ESM launcher. The proxy remains
 * provider-neutral: it waits until arguments reveal a host/guest mount, then
 * delegates to the same capability graph used for CommonJS exports.
 */
export function wrapImportedCapability(value: unknown): unknown {
  if ((!value || typeof value !== "object") && typeof value !== "function")
    return value;
  const object = value as object;
  const cached = importedCapabilityProxies.get(object);
  if (cached) return cached;
  const invoke = (
    callable: Function,
    receiver: unknown,
    args: unknown[],
  ): unknown => {
    const hostRoot = process.env["SUPERCOV_PROJECT_ROOT"];
    const mapping = hostRoot
      ? args.map((argument) => discoverWorkspaceMapping(argument, hostRoot)).find(Boolean)
      : undefined;
    if (mapping) {
      const fingerprint = process.env["SUPERCOV_EXECUTION_FINGERPRINT"] ?? "unversioned";
      const scopedArguments = args.map((argument) =>
        scopeCapabilityCache(argument, fingerprint),
      );
      const supervisedArguments = scopedArguments.map((entry) =>
        wrapCapabilityCallbacks(entry.value, mapping),
      );
      record({
        event: "workspace-capability",
        hostRoot: mapping.hostRoot,
        guestRoot: mapping.guestRoot,
        cacheIdentities: scopedArguments.flatMap((entry) => entry.changed),
      });
      return wrapResult(
        Reflect.apply(callable, receiver, supervisedArguments),
        mapping,
      );
    }
    return wrapImportedCapability(Reflect.apply(callable, receiver, args));
  };
  const proxy = new Proxy(object, {
    get(target, property) {
      const member = Reflect.get(target, property, target) as unknown;
      if (typeof member !== "function") return wrapImportedCapability(member);
      return (...args: unknown[]) => invoke(member, target, args);
    },
    ...(typeof value === "function"
      ? {
          apply(target: object, thisArgument: unknown, args: unknown[]) {
            return invoke(target as Function, thisArgument, args);
          },
          construct(target: object, args: unknown[], newTarget: Function): object {
            const result = Reflect.construct(target as Function, args, newTarget);
            return wrapImportedCapability(result) as object;
          },
        }
      : {}),
  });
  importedCapabilityProxies.set(object, proxy);
  return proxy;
}

function patchBuilder(builder: Function): void {
  if (patchedBuilders.has(builder)) return;
  const descriptor = Object.getOwnPropertyDescriptor(builder, "build");
  if (!descriptor || typeof descriptor.value !== "function" || !descriptor.writable)
    return;
  const original = descriptor.value as (...args: unknown[]) => unknown;
  Object.defineProperty(builder, "build", {
    ...descriptor,
    value: function supercovCapabilityBuild(...args: unknown[]) {
      const hostRoot = process.env["SUPERCOV_PROJECT_ROOT"];
      const mapping = hostRoot
        ? discoverWorkspaceMapping(args[0], hostRoot)
        : undefined;
      if (!mapping) return Reflect.apply(original, this, args);
      const fingerprint =
        process.env["SUPERCOV_EXECUTION_FINGERPRINT"] ?? "unversioned";
      const scoped = scopeCapabilityCache(args[0], fingerprint);
      const callArguments = [
        wrapCapabilityCallbacks(scoped.value, mapping),
        ...args.slice(1),
      ];
      record({
        event: "workspace-capability",
        hostRoot: mapping.hostRoot,
        guestRoot: mapping.guestRoot,
        cacheIdentities: scoped.changed,
      });
      return wrapResult(Reflect.apply(original, this, callArguments), mapping);
    },
  });
  patchedBuilders.add(builder);
}

function inspectExports(value: unknown, depth = 0): void {
  if (
    ((!value || typeof value !== "object") && typeof value !== "function") ||
    depth > 2
  )
    return;
  const object = value as object;
  if (exportedValues.has(object)) return;
  exportedValues.add(object);
  if (typeof value === "function") patchBuilder(value);
  for (const key of Reflect.ownKeys(object).slice(0, 200)) {
    if (key === "prototype" || key === "caller" || key === "callee") continue;
    try {
      const descriptor = Object.getOwnPropertyDescriptor(object, key);
      if (descriptor && "value" in descriptor)
        inspectExports(descriptor.value, depth + 1);
    } catch {
      // Export inspection is best-effort and never invokes accessors.
    }
  }
}

function childOptionsIndex(
  method: string,
  args: unknown[],
): number | undefined {
  if (method === "spawn" || method === "spawnSync" || method === "fork")
    return Array.isArray(args[1]) || (args.length > 2 && args[2] !== undefined)
      ? 2
      : 1;
  if (
    method === "exec" ||
    method === "execSync" ||
    method === "execFile" ||
    method === "execFileSync"
  )
    return Array.isArray(args[1]) || (args.length > 2 && args[2] !== undefined)
      ? 2
      : 1;
  return undefined;
}

function injectChildEnvironment(
  method: string,
  args: unknown[],
): unknown[] {
  const index = childOptionsIndex(method, args);
  if (index === undefined) return args;
  const next = [...args];
  const original = next[index];
  const options =
    original && typeof original === "object" && !Array.isArray(original)
      ? (original as UnknownRecord)
      : {};
  const inherited = coverageVariables(process.env);
  const existingEnvironment =
    options.env && typeof options.env === "object"
      ? (options.env as NodeJS.ProcessEnv)
      : undefined;
  if (existingEnvironment?.["SUPERCOV_INTERNAL_INSTRUMENTER"] === "1")
    return args;
  const environment = existingEnvironment
    ? { ...existingEnvironment, ...inherited }
    : { ...process.env, ...inherited };
  environment.NODE_OPTIONS = appendNodeImport(
    existingEnvironment?.NODE_OPTIONS ?? process.env.NODE_OPTIONS,
    pathToFileURL(resolve(process.env["SUPERCOV_PROJECT_ROOT"] ?? process.cwd(), ".supercov/register.mjs")).href,
  );
  next[index] = { ...options, env: environment };
  if (typeof original === "function") next.splice(index + 1, 0, original);
  record({
    event: "child-launch",
    method,
    command: safeArgument(String(args[0] ?? "")),
  });
  return next;
}

function patchChildProcesses(): void {
  const methods = [
    "spawn",
    "spawnSync",
    "exec",
    "execSync",
    "execFile",
    "execFileSync",
    "fork",
  ] as const;
  for (const method of methods) {
    const original = childProcess[method] as (...args: unknown[]) => unknown;
    Object.defineProperty(childProcess, method, {
      configurable: true,
      enumerable: true,
      writable: true,
      value: (...args: unknown[]) =>
        Reflect.apply(original, childProcess, injectChildEnvironment(method, args)),
    });
  }
  syncBuiltinESMExports();
}

export function installLaunchSupervisor(): void {
  if (installed) return;
  installed = true;
  patchChildProcesses();
  type ModuleLoad = (
    this: unknown,
    request: string,
    parent: { filename?: string } | undefined,
    isMain: boolean,
  ) => unknown;
  const moduleLoader = Module as unknown as { _load: ModuleLoad };
  const originalLoad = moduleLoader._load;
  moduleLoader._load = function supercovCapabilityLoad(
    request: string,
    parent: { filename?: string } | undefined,
    isMain: boolean,
  ) {
    const exports = originalLoad.call(this, request, parent, isMain) as unknown;
    inspectExports(exports);
    return exports;
  };
  record({
    event: "process",
    cwd: process.cwd(),
    command: commandSummary(process.argv),
    entrypoint: process.argv[1],
  });
}
