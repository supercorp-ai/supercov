import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

import {
  decodeCoverageScope,
  encodeCoverageScope,
} from "../../runtime/javascript/transport.mjs";
import { inferTestProvenance } from "../../runtime/javascript/provenance.mjs";
import {
  discoverWorkspaceMapping,
  guestCoverageEnvironment,
  scopeCapabilityCache,
  wrapCapabilityObject,
  wrapImportedCapability,
} from "../../runtime/javascript/launchSupervisor.mjs";

test("a phase is only honoured for the attempt that minted it", async () => {
  // A browser context shared by a whole worker keeps the previous test's last
  // phase in storage and in its cookie; tagging the next test's evidence with
  // it produced archives the engine rejected as referencing an unknown phase.
  const runtime = await import(pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href);
  assert.equal(runtime.phaseBelongsToAttempt("attempt-a:phase:2", "attempt-a"), true);
  assert.equal(runtime.phaseBelongsToAttempt("attempt-a:phase:2", "attempt-b"), false);
  assert.equal(runtime.phaseBelongsToAttempt("attempt-a:phase:2", "attempt"), false);
  assert.equal(runtime.phaseBelongsToAttempt(undefined, "attempt-a"), false);
  assert.equal(runtime.phaseBelongsToAttempt("attempt-a:phase:2", ""), false);
});

test("coverage scopes round-trip without losing worker, retry, or phase identity", () => {
  const scope = {
    version: 1,
    runId: "run",
    workerId: "worker-2",
    testId: "test",
    testKey: "key",
    retry: 3,
    attemptId: "attempt",
  };
  assert.deepEqual(decodeCoverageScope(encodeCoverageScope(scope)), scope);
});

test("test provenance prefers explicit, project, path, then runner defaults", () => {
  assert.equal(
    inferTestProvenance({ runner: "playwright", file: "tests/unit/a.ts" }).kind,
    "unit",
  );
  assert.equal(
    inferTestProvenance({
      runner: "playwright",
      file: "tests/unit/a.ts",
      project: "e2e-chromium",
    }).kind,
    "e2e",
  );
  assert.equal(
    inferTestProvenance({
      runner: "playwright",
      file: "tests/unit/a.ts",
      explicitKind: "integration",
    }).kind,
    "integration",
  );
});

test("remote launch mapping is provider-neutral and scopes reusable snapshots", () => {
  const mapping = discoverWorkspaceMapping(
    {
      arbitrary: {
        hostPath: "/host/project",
        guestPath: "/guest/workspace",
      },
    },
    "/host/project",
  );
  assert.deepEqual(mapping, {
    hostRoot: "/host/project",
    guestRoot: "/guest/workspace",
  });
  const scoped = scopeCapabilityCache(
    { snapshotTag: "base", unrelated: "unchanged" },
    "abcdef0123456789abcdef",
  );
  assert.match(scoped.value.snapshotTag, /^base-supercov-abcdef0123456789abcd$/);
  assert.equal(scoped.value.unrelated, "unchanged");
  const environment = guestCoverageEnvironment(mapping, {
    SUPERCOV_RUN_ID: "run",
    SUPERCOV_PROJECT_ROOT: "/host/project",
  });
  assert.equal(environment.SUPERCOV_PROJECT_ROOT, "/guest/workspace");
  assert.equal(environment.SUPERCOV_DURABLE_EVIDENCE_EACH_TEST, "1");
  assert.match(
    environment.NODE_OPTIONS,
    /\/guest\/workspace\/\.supercov\/node_modules\/register\.mjs/,
  );
});

test("capability proxies preserve frozen constructor and export properties", () => {
  class PrismaSessionStorage {}
  const wrappedConstructor = wrapImportedCapability(PrismaSessionStorage);
  assert.equal(wrappedConstructor, PrismaSessionStorage);
  assert.equal(wrappedConstructor.prototype, PrismaSessionStorage.prototype);
  assert.ok(new wrappedConstructor() instanceof PrismaSessionStorage);

  const registered = new Map([["session", wrappedConstructor]]);
  const instance = new PrismaSessionStorage();
  assert.equal(instance.constructor, registered.get("session"));

  class RestResource {}
  const exports = wrapImportedCapability({ RestResource });
  assert.equal(exports.RestResource, RestResource);
  assert.equal(exports.RestResource, exports.RestResource);
  class Product extends exports.RestResource {}
  assert.ok(new Product() instanceof RestResource);

  const capability = {};
  const fixed = () => "fixed";
  Object.defineProperty(capability, "fixed", {
    configurable: false,
    enumerable: true,
    writable: false,
    value: fixed,
  });
  const wrappedCapability = wrapCapabilityObject(capability, {
    hostRoot: "/host/project",
    guestRoot: "/guest/workspace",
  });
  assert.equal(wrappedCapability.fixed, fixed);
});

test("an execution-trace writer that meets a clone of itself rotates its log", {
  skip: process.platform === "win32",
}, () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-execution-clone-"));
  try {
    const supervisor = pathToFileURL(resolve("runtime/javascript/launchSupervisor.mjs")).href;
    const child = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `import { readdirSync, appendFileSync } from "node:fs";
         import { spawnSync } from "node:child_process";
         const { installLaunchSupervisor } = await import(${JSON.stringify(supervisor)});
         installLaunchSupervisor();
         spawnSync(process.execPath, ["-e", "0"]);
         const directory = ${JSON.stringify(root)};
         const [log] = readdirSync(directory).filter((name) => name.startsWith("execution."));
         appendFileSync(directory + "/" + log, JSON.stringify({ event: "from-a-clone" }) + String.fromCharCode(10));
         spawnSync(process.execPath, ["-e", "0"]);
         process.stdout.write(String(process.pid));`,
      ],
      {
        cwd: process.cwd(),
        env: { ...process.env, SUPERCOV_EXECUTION_LOG: resolve(root, "execution.jsonl") },
        encoding: "utf8",
      },
    );
    assert.equal(child.status, 0, child.stderr);
    // Spawned children record their own logs; only the parent's must rotate.
    const parentPid = child.stdout.trim();
    const logs = readdirSync(root)
      .filter((name) => name.startsWith(`execution.host.${parentPid}-`))
      .sort();
    assert.equal(logs.length, 2, `expected a rotated execution log after the clone, saw ${logs}`);
    for (const log of logs)
      for (const line of readFileSync(resolve(root, log), "utf8").trim().split("\n"))
        JSON.parse(line);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a background writer that meets a clone of itself moves to a fresh shard", {
  skip: process.platform === "win32",
}, () => {
  // Pool VMs restored from one snapshot run clones of the same process with
  // the same pid and the same cached shard path; their appends tear each
  // other's lines. Simulate the clone by appending foreign bytes to the shard
  // between two records: the second record must land in a new shard, and the
  // first shard must keep only what this writer wrote before the clone.
  const root = mkdtempSync(resolve(tmpdir(), "supercov-background-clone-"));
  try {
    const runtime = pathToFileURL(resolve("runtime/javascript/runtime.mjs")).href;
    const child = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `import { readdirSync, appendFileSync } from "node:fs";
         const runtime = await import(${JSON.stringify(runtime)});
         runtime.coverageHit("before-clone");
         const directory = ${JSON.stringify(resolve(root, "run-clone", "background"))};
         const [shard] = readdirSync(directory);
         appendFileSync(directory + "/" + shard, JSON.stringify({ type: "hit", id: "from-a-clone" }) + String.fromCharCode(10));
         runtime.coverageHit("after-clone");`,
      ],
      {
        cwd: process.cwd(),
        env: { ...process.env, SUPERCOV_RUN_ID: "run-clone", SUPERCOV_SERVER_EVIDENCE_ROOT: root },
        encoding: "utf8",
      },
    );
    assert.equal(child.status, 0, child.stderr);
    const directory = resolve(root, "run-clone", "background");
    const files = readdirSync(directory).sort();
    assert.equal(files.length, 2, `expected a second shard after the clone, saw ${files}`);
    const ids = files.map((file) =>
      readFileSync(resolve(directory, file), "utf8").trim().split("\n").map((line) => JSON.parse(line).id),
    );
    assert.deepEqual(ids.flat().sort(), ["after-clone", "before-clone", "from-a-clone"]);
    assert.ok(ids.some((shard) => shard.length === 1 && shard[0] === "after-clone"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("background evidence is durable before an uncatchable process death", {
  skip: process.platform === "win32",
}, () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-background-kill-"));
  try {
    const runtime = pathToFileURL(
      resolve("runtime/javascript/runtime.mjs"),
    ).href;
    const child = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `const runtime = await import(${JSON.stringify(runtime)}); runtime.coverageHit("kill-safe-hit"); process.kill(process.pid, "SIGKILL");`,
      ],
      {
        cwd: process.cwd(),
        env: {
          ...process.env,
          SUPERCOV_RUN_ID: "run-kill-safe",
          SUPERCOV_SERVER_EVIDENCE_ROOT: root,
        },
        encoding: "utf8",
      },
    );
    assert.equal(child.signal, "SIGKILL");
    const directory = resolve(root, "run-kill-safe", "background");
    const files = readdirSync(directory);
    assert.equal(files.length, 1);
    const records = readFileSync(resolve(directory, files[0]), "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert.equal(records.length, 1);
    assert.equal(records[0].id, "kill-safe-hit");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a loopback request that loses its carrier is retained as background evidence", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-loopback-background-"));
  try {
    const runtime = pathToFileURL(resolve("runtime/javascript/runtime.mjs")).href;
    const child = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `
          import http from "node:http";
          const runtime = await import(${JSON.stringify(runtime)});
          const scope = {
            version: 1,
            runId: "run-loopback",
            workerId: "worker-1",
            testId: "test-loopback",
            testKey: "test-loopback",
            retry: 0,
            attemptId: "attempt-loopback",
          };
          let origin;
          const handler = runtime.withRequestPhase(async (request, response) => {
            if (request.url === "/outer") {
              await runtime.withCoverageCarrier({ version: 1, scope }, async () => {
                runtime.coverageHit("outer-scoped-hit");
                await new Promise((resolve, reject) => {
                  // Deliberately bypass fetch propagation. The nested inbound
                  // request has headers but no Supercov carrier, matching an
                  // interceptor or SDK that performs a raw loopback dispatch.
                  const nested = http.request(origin + "/inner", { method: "POST" }, (result) => {
                    result.resume();
                    result.on("end", resolve);
                  });
                  nested.on("error", reject);
                  nested.end();
                });
              });
              response.end("outer");
              return;
            }
            runtime.coverageHit("inner-background-hit");
            response.end("inner");
          });
          const server = http.createServer(handler);
          await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
          origin = "http://127.0.0.1:" + server.address().port;
          await fetch(origin + "/outer");
          await new Promise((resolve) => server.close(resolve));
        `,
      ],
      {
        cwd: process.cwd(),
        env: {
          ...process.env,
          SUPERCOV_RUN_ID: "run-loopback",
          SUPERCOV_SERVER_EVIDENCE_ROOT: root,
        },
        encoding: "utf8",
      },
    );
    assert.equal(child.status, 0, child.stderr);
    const runDirectory = resolve(root, "run-loopback");
    const backgroundDirectory = resolve(runDirectory, "background");
    const background = readdirSync(backgroundDirectory).flatMap((file) =>
      readFileSync(resolve(backgroundDirectory, file), "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line)),
    );
    assert.ok(background.some((record) => record.id === "inner-background-hit"));
    const attemptsDirectory = resolve(runDirectory, "attempts");
    const scoped = readdirSync(attemptsDirectory)
      .filter((file) => file.endsWith(".jsonl"))
      .flatMap((file) =>
        readFileSync(
          resolve(attemptsDirectory, file),
          "utf8",
        )
          .trim()
          .split("\n")
          .map((line) => JSON.parse(line)),
      );
    assert.ok(scoped.some((record) => record.id === "outer-scoped-hit"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("instrumentation stack cleanup keeps the user's first frame", async () => {
  const runtime = await import("../../runtime/javascript/runtime.mjs");
  const error = new Error("assertion failed");
  error.stack = [
    "Error: assertion failed",
    "    at Proxy.toBe (/workspace/.supercov/playwright.js:660:44)",
    "    at Object.apply (/workspace/.supercov/nodeAssertAdapter.js:23:10)",
    "    at load (file:///workspace/.supercov/resolve-loader.mjs:42:7)",
    "    at withProbeV2Context (/workspace/.supercov/runtime.js:248:12)",
    "    at tests/offline/article.spec.ts:155:9",
    "    at userHelper (/workspace/tests/helper.ts:8:3)",
  ].join("\n");
  assert.equal(runtime.cleanInstrumentationStack(error), error);
  assert.equal(
    error.stack,
    [
      "Error: assertion failed",
      "    at tests/offline/article.spec.ts:155:9",
      "    at userHelper (/workspace/tests/helper.ts:8:3)",
    ].join("\n"),
  );
});

test("server evidence transport failure is explicit and fail-closed", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-transport-failure-"));
  try {
    const blocked = resolve(root, "not-a-directory");
    writeFileSync(blocked, "file blocks evidence directory creation");
    const runtime = pathToFileURL(resolve("runtime/javascript/runtime.mjs")).href;
    const child = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `const runtime = await import(${JSON.stringify(runtime)}); runtime.coverageHit("must-not-disappear");`,
      ],
      {
        cwd: process.cwd(),
        env: {
          ...process.env,
          SUPERCOV_RUN_ID: "run-transport-failure",
          SUPERCOV_SERVER_EVIDENCE_ROOT: blocked,
        },
        encoding: "utf8",
      },
    );
    assert.notEqual(child.status, 0);
    assert.match(child.stderr, /SUPERCOV_EVIDENCE_TRANSPORT_FAILED|could not persist coverage evidence/u);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
