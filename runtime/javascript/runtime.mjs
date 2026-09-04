var __defProp = Object.defineProperty;
var __defProps = Object.defineProperties;
var __getOwnPropDescs = Object.getOwnPropertyDescriptors;
var __getOwnPropSymbols = Object.getOwnPropertySymbols;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __propIsEnum = Object.prototype.propertyIsEnumerable;
var __defNormalProp = (obj, key, value) => key in obj ? __defProp(obj, key, { enumerable: true, configurable: true, writable: true, value }) : obj[key] = value;
var __spreadValues = (a, b) => {
  for (var prop in b || (b = {}))
    if (__hasOwnProp.call(b, prop))
      __defNormalProp(a, prop, b[prop]);
  if (__getOwnPropSymbols)
    for (var prop of __getOwnPropSymbols(b)) {
      if (__propIsEnum.call(b, prop))
        __defNormalProp(a, prop, b[prop]);
    }
  return a;
};
var __spreadProps = (a, b) => __defProps(a, __getOwnPropDescs(b));

// dist/transport.js
var COVERAGE_SCOPE_HEADER = "x-supercov-scope";
var COVERAGE_PHASE_HEADER = "x-supercov-phase";
var COVERAGE_SCOPE_COOKIE = "__supercov_scope";
var COVERAGE_PHASE_COOKIE = "__supercov_phase";
var COVERAGE_CARRIER_ENV = "SUPERCOV_CONTEXT";
var DEFAULT_SERVER_EVIDENCE_ROOT = "/tmp/supercov-server-evidence";
function configuredServerEvidenceRoot() {
  var _a8;
  return typeof process !== "undefined" && ((_a8 = process.env) == null ? void 0 : _a8["SUPERCOV_SERVER_EVIDENCE_ROOT"]) ? process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"] : DEFAULT_SERVER_EVIDENCE_ROOT;
}
function nonEmpty(value) {
  return typeof value === "string" && value.length > 0;
}
function safeKey(value) {
  return /^[a-zA-Z0-9_-]+$/.test(value);
}
function pathComponent(value) {
  const safe = value.replace(/[^a-zA-Z0-9_-]/g, "_");
  return safe || "unscoped";
}
function encodeCoverageScope(scope) {
  return new URLSearchParams({
    v: String(scope.version),
    r: scope.runId,
    w: scope.workerId,
    t: scope.testId,
    k: scope.testKey,
    a: String(scope.retry),
    i: scope.attemptId
  }).toString();
}
function decodeCoverageScope(encoded) {
  if (!encoded)
    return void 0;
  try {
    const values = new URLSearchParams(encoded);
    const runId = values.get("r");
    const workerId = values.get("w");
    const testId2 = values.get("t");
    const testKey = values.get("k");
    const attemptId = values.get("i");
    const retry = Number(values.get("a"));
    if (values.get("v") !== "1" || !nonEmpty(runId) || !nonEmpty(workerId) || !nonEmpty(testId2) || !nonEmpty(testKey) || !safeKey(testKey) || !nonEmpty(attemptId) || !safeKey(attemptId) || !Number.isSafeInteger(retry) || retry < 0)
      return void 0;
    return {
      version: 1,
      runId,
      workerId,
      testId: testId2,
      testKey,
      retry,
      attemptId
    };
  } catch (e) {
    return void 0;
  }
}
function encodeCoverageCarrier(carrier) {
  return Buffer.from(JSON.stringify(carrier), "utf8").toString("base64url");
}
function decodeCoverageCarrier(encoded) {
  if (!encoded)
    return void 0;
  try {
    const value = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
    if (value.version !== 1)
      return void 0;
    if (value.scope) {
      const roundTrip = decodeCoverageScope(encodeCoverageScope(value.scope));
      if (!roundTrip)
        return void 0;
    }
    if (value.phaseId !== void 0 && value.phaseId.length === 0)
      return void 0;
    return value;
  } catch (e) {
    return void 0;
  }
}
function serverRunEvidenceDirectory(runId, root = configuredServerEvidenceRoot()) {
  return `${root.replace(/\/+$/, "")}/${pathComponent(runId)}`;
}
function serverEvidenceDirectory(scope, root = configuredServerEvidenceRoot()) {
  return `${serverRunEvidenceDirectory(scope.runId, root)}/attempts`;
}
function serverEvidencePath(scope, root = configuredServerEvidenceRoot()) {
  return `${serverEvidenceDirectory(scope, root)}/${pathComponent(scope.attemptId)}.jsonl`;
}
function backgroundEvidenceDirectory(runId, root = configuredServerEvidenceRoot()) {
  return `${serverRunEvidenceDirectory(runId, root)}/background`;
}
function backgroundEvidencePath(runId, processId = typeof process === "undefined" ? "unknown" : String(process.pid), root = configuredServerEvidenceRoot()) {
  return `${backgroundEvidenceDirectory(runId, root)}/${pathComponent(processId)}.jsonl`;
}

// dist/runtime.js
var runtimeGlobal = globalThis;
var runtimeInstanceToken = "__SUPERCOV_RUNTIME_INSTANCE__";
var runtimeInstance = runtimeInstanceToken === "__SUPERCOV_RUNTIME_INSTANCE__" ? "application" : runtimeInstanceToken;
var _a;
var isBrowser = !(typeof process !== "undefined" && typeof ((_a = process.versions) == null ? void 0 : _a.node) === "string");
var _a2;
var testId = (_a2 = runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__) != null ? _a2 : "unscoped";
var storageKey = "__supercov_coverage_" + testId;
var phaseStorageKey = "__supercov_phase";
var pendingDefaults = /* @__PURE__ */ new Map();
function vectorKey(vector) {
  return vector.values.map((value) => value === null ? "-" : value ? "T" : "F").join("") + ":" + (vector.outcome ? "T" : "F");
}
function getFs() {
  if (isBrowser || typeof process === "undefined")
    return void 0;
  try {
    const getBuiltinModule = process.getBuiltinModule;
    return getBuiltinModule == null ? void 0 : getBuiltinModule("node:fs");
  } catch (e) {
    return void 0;
  }
}
function createState() {
  var _a8, _b, _c, _d;
  const state2 = {
    decisions: /* @__PURE__ */ new Map(),
    hits: /* @__PURE__ */ new Set(),
    events: [],
    eventKeys: /* @__PURE__ */ new Set(),
    bufferedAttempts: /* @__PURE__ */ new Set(),
    serverBuffers: /* @__PURE__ */ new Map(),
    persistedServerRecords: /* @__PURE__ */ new Set(),
    backgroundBuffers: /* @__PURE__ */ new Map(),
    backgroundWriters: /* @__PURE__ */ new Map(),
    backgroundShardSizes: /* @__PURE__ */ new Map(),
    backgroundSequence: 0,
    runtimeSnapshots: false,
    assertionPhases: /* @__PURE__ */ new Map(),
    probeV2Files: /* @__PURE__ */ new Set(),
    probeV2Clock: { epoch: Number.NaN, fast: false },
    probeV2ContextEpochs: /* @__PURE__ */ new Map(),
    probeV2NextEpoch: 1,
    probeV2HookInstalled: false,
    pendingServerAppends: /* @__PURE__ */ new Map(),
    createdEvidenceDirectories: /* @__PURE__ */ new Set(),
    serverFlushScheduled: false,
    serverExitHookInstalled: false,
    serverTransportFailure: void 0
  };
  if (!isBrowser)
    return state2;
  try {
    const stored = JSON.parse((_a8 = localStorage.getItem(storageKey)) != null ? _a8 : "{}");
    for (const snapshot of (_b = stored.decisions) != null ? _b : []) {
      state2.decisions.set(snapshot.meta.id, {
        meta: snapshot.meta,
        vectors: new Map(snapshot.vectors.map((vector) => [vectorKey(vector), vector]))
      });
    }
    for (const id of (_c = stored.hits) != null ? _c : [])
      state2.hits.add(id);
    for (const event of (_d = stored.events) != null ? _d : []) {
      state2.events.push(event);
      state2.eventKeys.add(eventKey(event));
    }
  } catch (e) {
  }
  return state2;
}
var _a3;
var runtimeStates = (_a3 = runtimeGlobal.__SUPERCOV_MCDC_STATES__) != null ? _a3 : /* @__PURE__ */ new Map();
runtimeGlobal.__SUPERCOV_MCDC_STATES__ = runtimeStates;
var _a4;
var state = (_a4 = runtimeStates.get(runtimeInstance)) != null ? _a4 : createState();
runtimeStates.set(runtimeInstance, state);
function createServerPhaseStorage() {
  var _a8;
  if (isBrowser || typeof process === "undefined")
    return void 0;
  try {
    const getBuiltinModule = process.getBuiltinModule;
    const AsyncLocalStorage = (_a8 = getBuiltinModule == null ? void 0 : getBuiltinModule("node:async_hooks")) == null ? void 0 : _a8.AsyncLocalStorage;
    return AsyncLocalStorage ? new AsyncLocalStorage() : void 0;
  } catch (e) {
    return void 0;
  }
}
var _a5;
var serverPhaseStorages = (_a5 = runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGES__) != null ? _a5 : /* @__PURE__ */ new Map();
runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGES__ = serverPhaseStorages;
var _a6;
var serverPhaseStorage = (_a6 = serverPhaseStorages.get(runtimeInstance)) != null ? _a6 : createServerPhaseStorage();
if (serverPhaseStorage)
  serverPhaseStorages.set(runtimeInstance, serverPhaseStorage);
function probeV2ContextKey(context) {
  var _a8, _b;
  const phase = (_a8 = context.phaseId) != null ? _a8 : "unscoped";
  if (!context.scope) {
    const runId = typeof process !== "undefined" ? process.env["SUPERCOV_RUN_ID"] : void 0;
    return `background\0${(_b = runId != null ? runId : runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__) != null ? _b : testId}\0${phase}`;
  }
  return `scope\0${attemptKey(context.scope)}\0${phase}`;
}
function activateProbeV2Key(key, force = false) {
  let epoch = force ? void 0 : state.probeV2ContextEpochs.get(key);
  if (epoch === void 0) {
    state.probeV2NextEpoch += 1;
    if (state.probeV2NextEpoch >= 4294967295) {
      state.probeV2NextEpoch = 1;
      state.probeV2ContextEpochs.clear();
      for (const file of state.probeV2Files) {
        file.hitEpochs.fill(0);
        file.decisionObservationEpochs.fill(0);
        file.decisionObservationCounts.fill(0);
        file.decisionCompleteEpochs.fill(0);
        for (const vectors of file.decisionEpochs) {
          if (vectors instanceof Uint32Array)
            vectors.fill(0);
          else
            vectors.clear();
        }
      }
    }
    epoch = state.probeV2NextEpoch;
    state.probeV2ContextEpochs.set(key, epoch);
  }
  state.probeV2Clock.epoch = epoch;
  return epoch;
}
function activateProbeV2Context(context) {
  return activateProbeV2Key(probeV2ContextKey(context));
}
function withProbeV2Context(context, callback) {
  const previous = state.probeV2Clock.epoch;
  activateProbeV2Context(context);
  try {
    return callback();
  } finally {
    state.probeV2Clock.epoch = previous;
  }
}
function installProbeV2AsyncHook() {
  var _a8;
  if (isBrowser || state.probeV2HookInstalled || typeof process === "undefined")
    return;
  try {
    const getBuiltinModule = process.getBuiltinModule;
    const createHook = (_a8 = getBuiltinModule == null ? void 0 : getBuiltinModule("node:async_hooks")) == null ? void 0 : _a8.createHook;
    if (!createHook)
      return;
    const epochs = [];
    createHook({
      before() {
        var _a9, _b;
        epochs.push(state.probeV2Clock.epoch);
        activateProbeV2Context((_b = (_a9 = serverPhaseStorage == null ? void 0 : serverPhaseStorage.getStore()) != null ? _a9 : environmentRequestContext()) != null ? _b : {});
      },
      after() {
        var _a9;
        state.probeV2Clock.epoch = (_a9 = epochs.pop()) != null ? _a9 : state.probeV2Clock.epoch;
      }
    }).enable();
    state.probeV2HookInstalled = true;
    state.probeV2Clock.fast = true;
  } catch (e) {
  }
}
runtimeGlobal.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__ = (browserTestId, browserPhaseId) => {
  state.probeV2Clock.fast = true;
  activateProbeV2Key(`browser\0${browserTestId}\0${browserPhaseId != null ? browserPhaseId : "unscoped"}`);
};
function decisionSnapshot() {
  return [...state.decisions.values()].map((decision) => ({
    meta: decision.meta,
    vectors: [...decision.vectors.values()]
  }));
}
function coverageSnapshot() {
  return {
    decisions: decisionSnapshot(),
    hits: [...state.hits],
    events: state.events
  };
}
function resetCoverage(testId2) {
  state.decisions.clear();
  state.hits.clear();
  state.events.length = 0;
  state.eventKeys.clear();
  state.probeV2ContextEpochs.clear();
  activateProbeV2Key(`reset\0${state.probeV2NextEpoch}`, true);
  if (testId2)
    runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ = testId2;
  if (isBrowser) {
    try {
      localStorage.removeItem(storageKey);
    } catch (e) {
    }
  }
}
runtimeGlobal.__SUPERCOV_MCDC_SNAPSHOT__ = decisionSnapshot;
runtimeGlobal.__SUPERCOV_COVERAGE_SNAPSHOT__ = coverageSnapshot;
runtimeGlobal.__SUPERCOV_RESET__ = resetCoverage;
var persistBrowserScheduled = false;
var persistBrowserListenersInstalled = false;
function persistBrowserNow() {
  if (!isBrowser)
    return;
  try {
    localStorage.setItem(storageKey, JSON.stringify(coverageSnapshot()));
  } catch (e) {
  }
}
function persistBrowser() {
  if (!isBrowser)
    return;
  // Persistence exists so evidence survives navigation, not to mirror every
  // event: serializing the whole snapshot per event is quadratic in a
  // render burst. Coalesce to one write per macrotask and flush when the
  // page is actually leaving.
  if (!persistBrowserListenersInstalled) {
    persistBrowserListenersInstalled = true;
    try {
      addEventListener("pagehide", persistBrowserNow);
      addEventListener("visibilitychange", () => {
        if (typeof document !== "undefined" && document.visibilityState === "hidden")
          persistBrowserNow();
      });
    } catch (e) {
    }
  }
  if (persistBrowserScheduled)
    return;
  persistBrowserScheduled = true;
  setTimeout(() => {
    persistBrowserScheduled = false;
    persistBrowserNow();
  }, 0);
}
function attemptKey(scope) {
  return `${scope.runId}\0${scope.workerId}\0${scope.attemptId}`;
}
function serverRecordKey(record) {
  var _a8;
  const suffix = record.type === "decision" ? `${record.meta.id}:${vectorKey(record.vector)}` : record.id;
  return `${(_a8 = record.phaseId) != null ? _a8 : "unscoped"}:${record.type}:${suffix}`;
}
function beginBufferedServerEvidence(scope) {
  if (isBrowser)
    return;
  const key = attemptKey(scope);
  state.bufferedAttempts.add(key);
  if (!state.serverBuffers.has(key))
    state.serverBuffers.set(key, {
      scope,
      directory: serverEvidenceDirectory(scope),
      path: serverEvidencePath(scope),
      records: /* @__PURE__ */ new Map()
    });
}
function flushBufferedServerEvidence(scope) {
  if (isBrowser)
    return void 0;
  const key = attemptKey(scope);
  state.bufferedAttempts.delete(key);
  const buffered = state.serverBuffers.get(key);
  if (!buffered)
    return void 0;
  state.serverBuffers.delete(key);
  const fs = getFs();
  if (!fs || buffered.records.size === 0)
    return void 0;
  try {
    fs.mkdirSync(buffered.directory, { recursive: true });
    fs.appendFileSync(buffered.path, [...buffered.records.values()].map((record) => JSON.stringify(record)).join("\n") + "\n");
    return buffered.path;
  } catch (e) {
    return void 0;
  }
}
function enableRuntimeSnapshotEvidence() {
  state.runtimeSnapshots = true;
}
function flushAllBufferedServerEvidence() {
  for (const buffered of [...state.serverBuffers.values()])
    flushBufferedServerEvidence(buffered.scope);
  for (const runId of [...state.backgroundBuffers.keys()])
    flushBufferedBackgroundEvidence(runId);
}
function flushBufferedBackgroundEvidence(runId) {
  if (isBrowser)
    return void 0;
  const records = state.backgroundBuffers.get(runId);
  if (!records || records.size === 0)
    return void 0;
  return state.backgroundWriters.get(runId);
}
function writeExclusiveBackgroundRecord(fs, runId, writer, initialSequence, payload) {
  let sequence = initialSequence;
  for (let attempt = 0; attempt < 1e4; attempt += 1) {
    const candidate = backgroundEvidencePath(runId, `${writer}-${sequence++}`);
    try {
      fs.writeFileSync(candidate, payload, { flag: "wx" });
      return sequence;
    } catch (error) {
      if (error.code === "EEXIST")
        continue;
      throw error;
    }
  }
  throw Object.assign(new Error("Could not allocate a collision-free Supercov background evidence record"), { code: "SUPERCOV_BACKGROUND_COLLISION_LIMIT" });
}
function backgroundWriterToken() {
  try {
    const getBuiltinModule = process.getBuiltinModule;
    const crypto = getBuiltinModule == null ? void 0 : getBuiltinModule("node:crypto");
    if (crypto)
      return crypto.randomBytes(4).toString("hex");
  } catch (e) {
  }
  return Math.floor(Math.random() * 4294967295).toString(16);
}
function appendDurableBackgroundRecord(fs, runId, record) {
  var _a8;
  const records = state.backgroundBuffers.get(runId) != null ? state.backgroundBuffers.get(runId) : /* @__PURE__ */ new Map();
  const key = serverRecordKey(record);
  if (records.has(key))
    return state.backgroundWriters.get(runId);
  const directory = backgroundEvidenceDirectory(runId);
  const payload = JSON.stringify(record) + "\n";
  fs.mkdirSync(directory, { recursive: true });
  let path = state.backgroundWriters.get(runId);
  // A pid is not an identity: pool VMs restored from one snapshot run clones
  // of this very process, same pid and same cached shard path, and their
  // appends over a shared mount tear each other's lines. The shard name gets
  // a fresh random token, and an append that finds the file a different size
  // than this writer left it has met a clone: it moves to a new shard.
  if (path) {
    let currentSize = -1;
    try {
      currentSize = fs.statSync(path).size;
    } catch (e) {
    }
    if (currentSize !== (state.backgroundShardSizes.get(path) ?? -1)) {
      path = void 0;
    }
  }
  if (!path) {
    const shard = (_a8 = process.env["SUPERCOV_EXECUTION_LOG_SHARD"]) != null ? _a8 : "process";
    const writer = `${shard}-${process.pid}-${backgroundWriterToken()}`;
    const nextSequence = writeExclusiveBackgroundRecord(fs, runId, writer, state.backgroundSequence, payload);
    state.backgroundSequence = nextSequence;
    path = backgroundEvidencePath(runId, `${writer}-${nextSequence - 1}`);
    state.backgroundWriters.set(runId, path);
    state.backgroundShardSizes.set(path, Buffer.byteLength(payload));
  } else {
    fs.appendFileSync(path, payload);
    state.backgroundShardSizes.set(path, (state.backgroundShardSizes.get(path) ?? 0) + Buffer.byteLength(payload));
  }
  records.set(key, record);
  state.backgroundBuffers.set(runId, records);
  return path;
}
var _a7;
// Evidence buffered for the current turn is lost when a signal ends the
// process, because "exit" does not run for one -- and killing a child in
// teardown is exactly how a suite stops a gateway it started. Flush on the
// terminating signals as well as on exit.
//
// Exactly one listener per signal serves every flusher: two of our own would
// each see a sibling listener, neither would judge itself alone, and a
// signalled process would stay alive. Having flushed, restore what the signal
// would have done -- if nothing else is listening the program expected to die,
// so re-raise with our listener gone; if the program installed its own handler
// it deliberately suppressed that default and we must not exit on its behalf.
function flushOnTermination(flush) {
  var _a9;
  if (typeof process === "undefined")
    return;
  const flushers = (_a9 = runtimeGlobal.__SUPERCOV_TERMINATION_FLUSHERS__) != null ? _a9 : /* @__PURE__ */ new Set();
  runtimeGlobal.__SUPERCOV_TERMINATION_FLUSHERS__ = flushers;
  flushers.add(flush);
  if (runtimeGlobal.__SUPERCOV_TERMINATION_INSTALLED__)
    return;
  runtimeGlobal.__SUPERCOV_TERMINATION_INSTALLED__ = true;
  const drain = () => {
    var _a10;
    for (const value of (_a10 = runtimeGlobal.__SUPERCOV_TERMINATION_FLUSHERS__) != null ? _a10 : []) {
      try {
        value();
      } catch (e) {
      }
    }
  };
  try {
    process.on("exit", drain);
  } catch (e) {
  }
  for (const signal of ["SIGTERM", "SIGINT", "SIGHUP", "SIGBREAK"]) {
    try {
      process.on(signal, () => {
        drain();
        if (process.listenerCount(signal) <= 1) {
          process.removeAllListeners(signal);
          process.kill(process.pid, signal);
        }
      });
    } catch (e) {
    }
  }
}
if (!isBrowser) {
  const flushers = (_a7 = runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__) != null ? _a7 : /* @__PURE__ */ new Set();
  runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__ = flushers;
  flushers.add(flushAllBufferedServerEvidence);
  if (!runtimeGlobal.__SUPERCOV_BUFFER_EXIT_INSTALLED__) {
    flushOnTermination(() => {
      var _a8;
      for (const flush of (_a8 = runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__) != null ? _a8 : [])
        flush();
    });
    runtimeGlobal.__SUPERCOV_BUFFER_EXIT_INSTALLED__ = true;
  }
}
function serverTransportError(runId, cause) {
  const detail = cause instanceof Error ? cause.message : String(cause);
  const failure = new Error(`Supercov could not persist coverage evidence for run ${runId}: ${detail}`);
  failure.code = "SUPERCOV_EVIDENCE_TRANSPORT_FAILED";
  failure.cause = cause;
  return failure;
}
function flushServerAppends() {
  const pending = state.pendingServerAppends;
  if (pending.size === 0)
    return;
  const fs = getFs();
  for (const [path, entry] of pending) {
    pending.delete(path);
    try {
      if (!fs)
        throw new Error("node:fs is unavailable");
      if (!state.createdEvidenceDirectories.has(entry.directory)) {
        fs.mkdirSync(entry.directory, { recursive: true });
        state.createdEvidenceDirectories.add(entry.directory);
      }
      fs.appendFileSync(path, entry.lines.join(""));
    } catch (cause) {
      const failure = serverTransportError(entry.runId, cause);
      state.serverTransportFailure = failure;
      throw failure;
    }
  }
}
function enqueueServerAppend(directory, path, line, runId) {
  // Fail closed with the original context: after one transport failure the
  // very next probe re-raises it synchronously.
  if (state.serverTransportFailure)
    throw state.serverTransportFailure;
  const entry = state.pendingServerAppends.get(path);
  if (entry) {
    entry.lines.push(line);
    if (entry.lines.length >= 2048)
      flushServerAppends();
  } else {
    state.pendingServerAppends.set(path, { directory, runId, lines: [line] });
  }
  if (!state.serverExitHookInstalled && typeof process !== "undefined") {
    state.serverExitHookInstalled = true;
    try {
      flushOnTermination(flushServerAppends);
    } catch (e) {
    }
  }
  // One synchronous write per event-loop turn instead of one per probe: a
  // request handler's burst of first-touch records becomes a single append
  // that is still on disk before the process yields past this turn.
  if (!state.serverFlushScheduled) {
    state.serverFlushScheduled = true;
    queueMicrotask(() => {
      state.serverFlushScheduled = false;
      flushServerAppends();
    });
  }
}
function appendServer(record) {
  var _a8, _b, _c, _d;
  if (state.runtimeSnapshots) {
    const timestampMs = (_a8 = record.timestampMs) != null ? _a8 : Date.now();
    if (record.type === "decision") {
      recordBrowserEvent(__spreadProps(__spreadValues({
        type: "decision",
        id: record.meta.id,
        vector: record.vector,
        timestampMs
      }, record.phaseId ? { phaseId: record.phaseId } : {}), {
        environment: "server"
      }));
    } else {
      recordBrowserEvent(__spreadProps(__spreadValues({
        type: "hit",
        id: record.id,
        timestampMs
      }, record.phaseId ? { phaseId: record.phaseId } : {}), {
        environment: "server"
      }));
    }
    return;
  }
  const fs = getFs();
  if (!fs)
    return;
  const context = currentRequestContext();
  const scope = context.scope;
  const runId = (_b = scope == null ? void 0 : scope.runId) != null ? _b : typeof process !== "undefined" ? process.env["SUPERCOV_RUN_ID"] : void 0;
  if (!runId)
    return;
  try {
    const directory = scope ? serverEvidenceDirectory(scope) : void 0;
    const path = scope ? serverEvidencePath(scope) : void 0;
    const serialized = __spreadValues(__spreadValues({}, record), scope ? { scope } : {});
    if (scope && state.bufferedAttempts.has(attemptKey(scope))) {
      const key = attemptKey(scope);
      const buffered = (_c = state.serverBuffers.get(key)) != null ? _c : {
        scope,
        directory,
        path,
        records: /* @__PURE__ */ new Map()
      };
      buffered.records.set(serverRecordKey(serialized), serialized);
      state.serverBuffers.set(key, buffered);
      return;
    }
    const deduplicationKey = scope && serialized.phaseId ? `${attemptKey(scope)}:${serverRecordKey(serialized)}` : void 0;
    if (deduplicationKey && state.persistedServerRecords.has(deduplicationKey))
      return;
    if (!path) {
      appendDurableBackgroundRecord(fs, runId, serialized);
      return;
    }
    enqueueServerAppend(directory, path, JSON.stringify(serialized) + "\n", runId);
    if (deduplicationKey)
      state.persistedServerRecords.add(deduplicationKey);
  } catch (cause) {
    if (cause instanceof Error && cause.code === "SUPERCOV_EVIDENCE_TRANSPORT_FAILED")
      throw cause;
    throw serverTransportError(runId, cause);
  }
}
function environmentRequestContext() {
  if (isBrowser || typeof process === "undefined")
    return void 0;
  const carrier = decodeCoverageCarrier(process.env[COVERAGE_CARRIER_ENV]);
  return carrier ? __spreadValues(__spreadValues({}, carrier.scope ? { scope: carrier.scope } : {}), carrier.phaseId ? { phaseId: carrier.phaseId } : {}) : void 0;
}
function currentRequestContext() {
  const stored = serverPhaseStorage == null ? void 0 : serverPhaseStorage.getStore();
  if (stored !== void 0) {
    if (stored.scope || stored.phaseId)
      return stored;
    return runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ ? { scope: runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ } : {};
  }
  const environment = environmentRequestContext();
  if ((environment == null ? void 0 : environment.scope) || (environment == null ? void 0 : environment.phaseId))
    return environment;
  return runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ ? { scope: runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ } : {};
}
function activateCoverageScope(scope) {
  if (scope)
    runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ = scope;
  else
    delete runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__;
  activateProbeV2Context(scope ? { scope } : {});
}
function coverageCarrier() {
  const context = currentRequestContext();
  return __spreadValues(__spreadValues({
    version: 1
  }, context.scope ? { scope: context.scope } : {}), context.phaseId ? { phaseId: context.phaseId } : {});
}
function withCoverageCarrier(carrier, callback) {
  const decoded = typeof carrier === "string" ? decodeCoverageCarrier(carrier) : carrier;
  if (!serverPhaseStorage || !decoded)
    return callback();
  const context = __spreadValues(__spreadValues({}, decoded.scope ? { scope: decoded.scope } : {}), decoded.phaseId ? { phaseId: decoded.phaseId } : {});
  return serverPhaseStorage.run(context, () => withProbeV2Context(context, callback));
}
function assertionPhaseState(scope) {
  const key = attemptKey(scope);
  const existing = state.assertionPhases.get(key);
  if (existing)
    return existing;
  const created = { counter: 0, phases: [] };
  state.assertionPhases.set(key, created);
  return created;
}
function finishAssertionPhase(phase, error) {
  phase.endedAtMs = Date.now();
  phase.status = error === void 0 ? "passed" : "failed";
  if (error !== void 0)
    phase.error = error instanceof Error ? error.message : String(error);
}

function cleanInstrumentationStack(error) {
  if (!error || typeof error !== "object" || typeof error.stack !== "string")
    return error;
  const lines = error.stack.split("\n");
  const visible = lines.filter((line, index) => index === 0 || !/[\\/]\.supercov[\\/](?:playwright|nodeTest|vitest|runtime|launchSupervisor|nodeAssert|nodeAssertStrict|nodeAssertAdapter|register|resolve-loader)\.(?:js|mjs)(?::|\))/u.test(line));
  if (visible.length !== lines.length) {
    try {
      error.stack = visible.join("\n");
    } catch (e) {
    }
  }
  return error;
}
function withNodeAssertionPhase(operation, source, callback) {
  var _a8;
  const context = currentRequestContext();
  const scope = context.scope;
  if (!scope)
    return callback();
  const existing = context.phaseId ? assertionPhaseState(scope).phases.find((phase2) => phase2.id === context.phaseId && phase2.kind === "assertion") : void 0;
  if (existing)
    return callback();
  const bridged = (_a8 = runtimeGlobal.__SUPERCOV_ASSERTION_PHASE_BRIDGE__) == null ? void 0 : _a8.call(runtimeGlobal, operation, source, callback);
  if (bridged == null ? void 0 : bridged.handled)
    return bridged.value;
  const attempt = assertionPhaseState(scope);
  const phase = __spreadProps(__spreadValues({
    id: `${scope.attemptId}:assertion:${++attempt.counter}`,
    kind: "assertion",
    operation
  }, source ? { source } : {}), {
    startedAtMs: Date.now()
  });
  attempt.phases.push(phase);
  try {
    const result = withCoverageCarrier({ version: 1, scope, phaseId: phase.id }, callback);
    if (result && typeof result.then === "function")
      return Promise.resolve(result).then((value) => {
        finishAssertionPhase(phase);
        return value;
      }, (error) => {
        finishAssertionPhase(phase, error);
        throw cleanInstrumentationStack(error);
      });
    finishAssertionPhase(phase);
    return result;
  } catch (error) {
    finishAssertionPhase(phase, error);
    throw cleanInstrumentationStack(error);
  }
}
function takeNodeAssertionPhases(scope) {
  var _a8, _b;
  const key = attemptKey(scope);
  const phases = (_b = (_a8 = state.assertionPhases.get(key)) == null ? void 0 : _a8.phases) != null ? _b : [];
  state.assertionPhases.delete(key);
  return phases;
}
function bindCoverageContext(callback, carrier = coverageCarrier()) {
  return function boundCoverageContext(...args) {
    return withCoverageCarrier(carrier, () => Reflect.apply(callback, this, args));
  };
}
function coverageContextHeaders() {
  const context = currentRequestContext();
  if (!context.scope)
    return {};
  return __spreadValues({
    [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(context.scope)
  }, context.phaseId ? { [COVERAGE_PHASE_HEADER]: context.phaseId } : {});
}
function coverageContextEnvironment() {
  return { [COVERAGE_CARRIER_ENV]: encodeCoverageCarrier(coverageCarrier()) };
}
function installServerFetchPropagation() {
  if (isBrowser || runtimeGlobal.__SUPERCOV_FETCH_PATCHED__ || typeof globalThis.fetch !== "function")
    return;
  const originalFetch = globalThis.fetch.bind(globalThis);
  globalThis.fetch = ((input, init) => {
    var _a8;
    const coverage = coverageContextHeaders();
    if (Object.keys(coverage).length === 0)
      return originalFetch(input, init);
    const headers = new Headers((_a8 = init == null ? void 0 : init.headers) != null ? _a8 : input instanceof Request ? input.headers : void 0);
    for (const [name, value] of Object.entries(coverage))
      headers.set(name, value);
    return originalFetch(input, __spreadProps(__spreadValues({}, init), { headers }));
  });
  runtimeGlobal.__SUPERCOV_FETCH_PATCHED__ = true;
}
installServerFetchPropagation();
function installServerChildPropagation() {
  var _a8;
  if (isBrowser || runtimeGlobal.__SUPERCOV_CHILD_PATCHED__ || typeof process === "undefined")
    return;
  const getBuiltinModule = process.getBuiltinModule;
  const child = getBuiltinModule == null ? void 0 : getBuiltinModule("node:child_process");
  if (!child)
    return;
  const mutableChild = child;
  const optionIndex = (method, args) => {
    if (method === "spawn" || method === "spawnSync" || method === "fork" || method === "execFile" || method === "execFileSync")
      return Array.isArray(args[1]) || args.length > 2 && args[2] !== void 0 ? 2 : 1;
    return 1;
  };
  for (const method of [
    "exec",
    "execFile",
    "execFileSync",
    "execSync",
    "fork",
    "spawn",
    "spawnSync"
  ]) {
    const original = mutableChild[method];
    if (typeof original !== "function")
      continue;
    mutableChild[method] = function(...args) {
      var _a9;
      const index = optionIndex(method, args);
      const existing = args[index] && typeof args[index] === "object" ? args[index] : {};
      const options = __spreadProps(__spreadValues({}, existing), {
        env: __spreadValues(__spreadValues(__spreadValues({}, process.env), (_a9 = existing.env) != null ? _a9 : {}), coverageContextEnvironment())
      });
      const scoped = [...args];
      if (typeof scoped[index] === "function")
        scoped.splice(index, 0, options);
      else
        scoped[index] = options;
      return Reflect.apply(original, child, scoped);
    };
  }
  const moduleBuiltin = getBuiltinModule == null ? void 0 : getBuiltinModule("node:module");
  (_a8 = moduleBuiltin == null ? void 0 : moduleBuiltin.syncBuiltinESMExports) == null ? void 0 : _a8.call(moduleBuiltin);
  runtimeGlobal.__SUPERCOV_CHILD_PATCHED__ = true;
}
installServerChildPropagation();
/**
 * Phase ids are minted as `<attemptId>:phase:<n>`, so a phase can be checked
 * against the attempt it claims to belong to without a lookup.
 */
function phaseBelongsToAttempt(phaseId, attemptId) {
  return typeof phaseId === "string" && typeof attemptId === "string" && attemptId.length > 0 && phaseId.startsWith(`${attemptId}:phase:`);
}
function currentPhaseId() {
  if (runtimeGlobal.__SUPERCOV_PHASE_ID__)
    return runtimeGlobal.__SUPERCOV_PHASE_ID__;
  if (!isBrowser)
    return currentRequestContext().phaseId;
  try {
    // The stored phase is per origin, so in a browser context that outlives
    // its test (a persistent profile shared by a whole worker) it still holds
    // the previous test's last phase when the next test's document loads. A
    // phase from another attempt would tag this test's evidence with a phase
    // it never reported, which the archive rightly rejects; only a phase of
    // the current attempt is honoured.
    const local = localStorage.getItem(phaseStorageKey);
    const attemptId = runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ != null ? runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ : testId;
    if (local && phaseBelongsToAttempt(local, attemptId))
      return local;
    return void 0;
  } catch (e) {
    return void 0;
  }
}
function requestHeaders(value) {
  if (!value || typeof value !== "object")
    return void 0;
  const directHeaders = value.headers;
  const request = directHeaders && typeof directHeaders === "object" ? value : value.request;
  if (!request || typeof request !== "object")
    return void 0;
  const headers = request.headers;
  if (!headers || typeof headers !== "object")
    return void 0;
  const get = headers.get;
  if (typeof get === "function")
    return {
      get(name) {
        return Reflect.apply(get, headers, [name]);
      }
    };
  const values = headers;
  return Object.keys(values).length > 0 ? {
    get(name) {
      var _a8;
      return (_a8 = values[name]) != null ? _a8 : values[name.toLowerCase()];
    }
  } : void 0;
}
function requestCoverageContext(value) {
  var _a8, _b;
  const headers = requestHeaders(value);
  if (!headers)
    return void 0;
  const rawCookie = headers.get("cookie");
  const cookies = /* @__PURE__ */ new Map();
  if (typeof rawCookie === "string") {
    for (const part of rawCookie.split(";")) {
      const separator = part.indexOf("=");
      if (separator < 0)
        continue;
      const name = part.slice(0, separator).trim();
      const encoded = part.slice(separator + 1).trim();
      try {
        cookies.set(name, decodeURIComponent(encoded));
      } catch (e) {
      }
    }
  }
  const encodedScope = (_a8 = headers.get(COVERAGE_SCOPE_HEADER)) != null ? _a8 : cookies.get(COVERAGE_SCOPE_COOKIE);
  const rawPhaseId = (_b = headers.get(COVERAGE_PHASE_HEADER)) != null ? _b : cookies.get(COVERAGE_PHASE_COOKIE);
  const scope = decodeCoverageScope(typeof encodedScope === "string" ? encodedScope : void 0);
  // The phase cookie outlives a test in a shared browser context just like
  // the stored phase does (see currentPhaseId); a request carrying a scope
  // and a phase from different attempts keeps the scope and drops the phase.
  const phaseId = typeof rawPhaseId === "string" && rawPhaseId.length > 0 && (!scope || phaseBelongsToAttempt(rawPhaseId, scope.attemptId)) ? rawPhaseId : void 0;
  return __spreadValues(__spreadValues({}, scope ? { scope } : {}), phaseId ? { phaseId } : {});
}
function withRequestPhase(handler) {
  if (!serverPhaseStorage)
    return handler;
  return function coverageRequestPhase(...args) {
    var _a8, _b, _c, _d;
    const requestContext = args.map((argument) => requestCoverageContext(argument)).find((context2) => context2 !== void 0);
    const inheritedContext = requestContext === void 0 ? currentRequestContext() : {};
    const context = __spreadValues(__spreadValues({}, ((_a8 = requestContext == null ? void 0 : requestContext.scope) != null ? _a8 : inheritedContext.scope) ? { scope: (_b = requestContext == null ? void 0 : requestContext.scope) != null ? _b : inheritedContext.scope } : {}), ((_c = requestContext == null ? void 0 : requestContext.phaseId) != null ? _c : inheritedContext.phaseId) ? { phaseId: (_d = requestContext == null ? void 0 : requestContext.phaseId) != null ? _d : inheritedContext.phaseId } : {});
    const invoke = () => Reflect.apply(handler, this, args);
    return requestContext !== void 0 || context.scope || context.phaseId ? serverPhaseStorage.run(context, () => withProbeV2Context(context, invoke)) : invoke();
  };
}
function eventKey(event) {
  var _a8;
  const suffix = event.type === "decision" ? `${event.id}:${vectorKey(event.vector)}` : event.id;
  return `${(_a8 = event.phaseId) != null ? _a8 : "unscoped"}:${event.type}:${suffix}`;
}
function recordBrowserEvent(event) {
  const key = eventKey(event);
  if (state.eventKeys.has(key))
    return false;
  state.eventKeys.add(key);
  state.events.push(event);
  return true;
}
function coverageHit(id) {
  state.hits.add(id);
  const timestampMs = Date.now();
  const phaseId = currentPhaseId();
  if (isBrowser) {
    if (recordBrowserEvent(__spreadProps(__spreadValues({
      type: "hit",
      id,
      timestampMs
    }, phaseId ? { phaseId } : {}), {
      environment: "browser"
    })))
      persistBrowser();
  } else {
    appendServer(__spreadValues({
      type: "hit",
      id,
      timestampMs
    }, phaseId ? { phaseId } : {}));
  }
}
function registerProbeV2(definition) {
  var _a8, _b;
  const file = {
    decisions: definition.decisions,
    pointIds: definition.pointIds,
    clock: state.probeV2Clock,
    hitEpochs: new Uint32Array(definition.pointIds.length),
    decisionEpochs: definition.decisions.map((meta) => meta.conditions.length <= 6 ? new Uint32Array(2 * 3 ** meta.conditions.length) : /* @__PURE__ */ new Map()),
    decisionVectorCounts: definition.decisions.map((_, index) => {
      var _a9, _b2;
      return (_b2 = (_a9 = definition.decisionVectorCounts) == null ? void 0 : _a9[index]) != null ? _b2 : 0;
    }),
    decisionObservationEpochs: new Uint32Array(definition.decisions.length),
    decisionObservationCounts: new Uint16Array(definition.decisions.length),
    decisionCompleteEpochs: new Uint32Array(definition.decisions.length)
  };
  state.probeV2Files.add(file);
  if (isBrowser) {
    state.probeV2Clock.fast = true;
    activateProbeV2Key(`browser\0${(_a8 = runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__) != null ? _a8 : testId}\0${(_b = runtimeGlobal.__SUPERCOV_PHASE_ID__) != null ? _b : "unscoped"}`);
  } else {
    installProbeV2AsyncHook();
  }
  return file;
}
function coverageHitV2(file, index) {
  const id = file.pointIds[index];
  if (!id)
    return;
  const fallbackEpoch = !file.clock.fast;
  const previousEpoch = file.clock.epoch;
  if (fallbackEpoch)
    activateProbeV2Context(currentRequestContext());
  try {
    const epoch = file.clock.epoch;
    if (file.hitEpochs[index] === epoch)
      return;
    file.hitEpochs[index] = epoch;
    coverageHit(id);
  } finally {
    if (fallbackEpoch)
      file.clock.epoch = previousEpoch;
  }
}
function decodeProbeV2Vector(conditionCount, encoded, outcome) {
  if (!Number.isSafeInteger(conditionCount) || conditionCount < 0 || conditionCount > 32 || !Number.isSafeInteger(encoded) || encoded < 0)
    return void 0;
  const values = [];
  let remaining = encoded;
  for (let index = 0; index < conditionCount; index += 1) {
    const digit = remaining % 3;
    values.push(digit === 0 ? null : digit === 2);
    remaining = Math.floor(remaining / 3);
  }
  return remaining === 0 ? { values, outcome } : void 0;
}
function mcdcEndV2(file, decisionIndex, encoded, value) {
  var _a8, _b;
  const meta = file.decisions[decisionIndex];
  if (!meta || !Number.isSafeInteger(encoded) || encoded < 0)
    return value;
  const outcome = Boolean(value);
  const vectorIndex = encoded * 2 + (outcome ? 1 : 0);
  if (!Number.isSafeInteger(vectorIndex))
    return value;
  const fallbackEpoch = !file.clock.fast;
  const previousEpoch = file.clock.epoch;
  if (fallbackEpoch)
    activateProbeV2Context(currentRequestContext());
  try {
    const epoch = file.clock.epoch;
    const seen = file.decisionEpochs[decisionIndex];
    if (!seen)
      return value;
    if (seen instanceof Uint32Array) {
      if (seen[vectorIndex] === epoch)
        return value;
      seen[vectorIndex] = epoch;
    } else {
      if (seen.get(vectorIndex) === epoch)
        return value;
      seen.set(vectorIndex, epoch);
    }
    const expectedCount = (_a8 = file.decisionVectorCounts[decisionIndex]) != null ? _a8 : 0;
    if (expectedCount > 0) {
      if (file.decisionObservationEpochs[decisionIndex] !== epoch) {
        file.decisionObservationEpochs[decisionIndex] = epoch;
        file.decisionObservationCounts[decisionIndex] = 0;
      }
      const observedCount = ((_b = file.decisionObservationCounts[decisionIndex]) != null ? _b : 0) + 1;
      file.decisionObservationCounts[decisionIndex] = observedCount;
      if (observedCount >= expectedCount)
        file.decisionCompleteEpochs[decisionIndex] = epoch;
    }
    if (!state.decisions.has(meta.id))
      state.decisions.set(meta.id, { meta, vectors: /* @__PURE__ */ new Map() });
    const vector = decodeProbeV2Vector(meta.conditions.length, encoded, outcome);
    return vector ? mcdcEnd({ meta, values: vector.values }, value) : value;
  } finally {
    if (fallbackEpoch)
      file.clock.epoch = previousEpoch;
  }
}
function selectionBegin(shortId, rightId) {
  return { shortId, rightId, rightEvaluated: false };
}
function applyInferredName(value, inferredName) {
  if (inferredName && typeof value === "function" && value.name === "") {
    Object.defineProperty(value, "name", {
      value: inferredName,
      configurable: true
    });
  }
  return value;
}
var hostNamesParenthesizedAssignments = (() => {
  let candidate;
  candidate = function() {
  };
  return candidate.name === "candidate";
})();
function parenthesizedAssignmentValue(value, inferredName) {
  return hostNamesParenthesizedAssignments ? applyInferredName(value, inferredName) : value;
}
function selectionRight(frame, value, inferredName) {
  frame.rightEvaluated = true;
  return applyInferredName(value, inferredName);
}
function selectionEnd(frame, value) {
  coverageHit(frame.rightEvaluated ? frame.rightId : frame.shortId);
  return value;
}
function optionalSelect(shortId, continuedId, value) {
  coverageHit(value === null || value === void 0 ? shortId : continuedId);
  return value;
}
function optionalCallBegin(shortId, continuedId) {
  return { shortId, continuedId, reached: false, continued: false };
}
function optionalCallReached(frame, value) {
  frame.reached = true;
  return value;
}
var optionalCallEmptySpread = {
  [Symbol.iterator]() {
    return {
      next() {
        return { done: true, value: void 0 };
      }
    };
  }
};
function optionalCallContinued(frame) {
  frame.continued = true;
  return optionalCallEmptySpread;
}
function optionalCallEnd(frame, value) {
  if (frame.reached)
    coverageHit(frame.continued ? frame.continuedId : frame.shortId);
  return value;
}
function defaultSelected(defaultId, value, inferredName) {
  var _a8;
  pendingDefaults.set(defaultId, ((_a8 = pendingDefaults.get(defaultId)) != null ? _a8 : 0) + 1);
  return applyInferredName(value, inferredName);
}
function defaultEntered(defaultId, providedId) {
  var _a8;
  const pending = (_a8 = pendingDefaults.get(defaultId)) != null ? _a8 : 0;
  if (pending > 0) {
    pendingDefaults.set(defaultId, pending - 1);
    coverageHit(defaultId);
  } else {
    coverageHit(providedId);
  }
}
function tryBegin(successId, catchId) {
  return { successId, catchId, caught: false };
}
function tryCatch(frame, value) {
  frame.caught = true;
  return value;
}
function tryEnd(frame) {
  coverageHit(frame.caught ? frame.catchId : frame.successId);
}
function loopBegin(zeroId, enteredId) {
  return { zeroId, enteredId, entered: false };
}
function loopEntered(frame) {
  frame.entered = true;
}
function loopEnd(frame) {
  coverageHit(frame.entered ? frame.enteredId : frame.zeroId);
}
function mcdcBegin(id, meta) {
  if (!state.decisions.has(id)) {
    state.decisions.set(id, { meta, vectors: /* @__PURE__ */ new Map() });
  }
  return {
    meta,
    values: Array.from({ length: meta.conditions.length }, () => null)
  };
}
function mcdcCondition(frame, index, value) {
  frame.values[index] = Boolean(value);
  return value;
}
function mcdcEnd(frame, value) {
  const decision = state.decisions.get(frame.meta.id);
  if (!decision)
    return value;
  const vector = { values: frame.values, outcome: Boolean(value) };
  const key = vectorKey(vector);
  decision.vectors.set(key, vector);
  const timestampMs = Date.now();
  const phaseId = currentPhaseId();
  if (isBrowser) {
    if (recordBrowserEvent(__spreadProps(__spreadValues({
      type: "decision",
      id: decision.meta.id,
      vector,
      timestampMs
    }, phaseId ? { phaseId } : {}), {
      environment: "browser"
    })))
      persistBrowser();
  } else {
    appendServer(__spreadValues({
      type: "decision",
      meta: decision.meta,
      vector,
      timestampMs
    }, phaseId ? { phaseId } : {}));
  }
  return value;
}
const directRuntimeApi = {
  activateCoverageScope,
  beginBufferedServerEvidence,
  bindCoverageContext,
  coverageCarrier,
  coverageContextEnvironment,
  coverageContextHeaders,
  cleanInstrumentationStack,
  coverageHit,
  coverageHitV2,
  coverageSnapshot,
  decodeProbeV2Vector,
  defaultEntered,
  defaultSelected,
  enableRuntimeSnapshotEvidence,
  flushBufferedBackgroundEvidence,
  flushBufferedServerEvidence,
  loopBegin,
  loopEnd,
  loopEntered,
  mcdcBegin,
  mcdcCondition,
  mcdcEnd,
  mcdcEndV2,
  optionalCallBegin,
  optionalCallContinued,
  optionalCallEnd,
  optionalCallReached,
  optionalSelect,
  parenthesizedAssignmentValue,
  phaseBelongsToAttempt,
  registerProbeV2,
  resetCoverage,
  selectionBegin,
  selectionEnd,
  selectionRight,
  takeNodeAssertionPhases,
  tryBegin,
  tryCatch,
  tryEnd,
  withCoverageCarrier,
  withNodeAssertionPhase,
  withRequestPhase,
  writeExclusiveBackgroundRecord
};
globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= directRuntimeApi;
if (typeof process !== "undefined")
  process.__SUPERCOV_DIRECT_RUNTIME__ ??= directRuntimeApi;
export {
  activateCoverageScope,
  beginBufferedServerEvidence,
  bindCoverageContext,
  coverageCarrier,
  coverageContextEnvironment,
  coverageContextHeaders,
  cleanInstrumentationStack,
  coverageHit,
  coverageHitV2,
  coverageSnapshot,
  decodeProbeV2Vector,
  defaultEntered,
  defaultSelected,
  enableRuntimeSnapshotEvidence,
  flushBufferedBackgroundEvidence,
  flushBufferedServerEvidence,
  flushOnTermination,
  loopBegin,
  loopEnd,
  loopEntered,
  mcdcBegin,
  mcdcCondition,
  mcdcEnd,
  mcdcEndV2,
  optionalCallBegin,
  optionalCallContinued,
  optionalCallEnd,
  optionalCallReached,
  optionalSelect,
  parenthesizedAssignmentValue,
  phaseBelongsToAttempt,
  registerProbeV2,
  resetCoverage,
  selectionBegin,
  selectionEnd,
  selectionRight,
  takeNodeAssertionPhases,
  tryBegin,
  tryCatch,
  tryEnd,
  withCoverageCarrier,
  withNodeAssertionPhase,
  withRequestPhase,
  writeExclusiveBackgroundRecord
};
