import assert from "node:assert/strict";
import test from "node:test";

import {
  decodeCoverageScope,
  encodeCoverageScope,
} from "../../runtime/javascript/transport.js";
import { inferTestProvenance } from "../../runtime/javascript/provenance.js";
import {
  discoverWorkspaceMapping,
  guestCoverageEnvironment,
  scopeCapabilityCache,
  wrapCapabilityObject,
  wrapImportedCapability,
} from "../../runtime/javascript/launchSupervisor.js";

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
  assert.match(environment.NODE_OPTIONS, /\/guest\/workspace\/\.supercov\/register\.mjs/);
});

test("capability proxies preserve frozen constructor and export properties", () => {
  class PrismaSessionStorage {}
  const wrappedConstructor = wrapImportedCapability(PrismaSessionStorage);
  assert.equal(wrappedConstructor.prototype, PrismaSessionStorage.prototype);
  assert.ok(new wrappedConstructor() instanceof PrismaSessionStorage);

  class RestResource {}
  const exports = wrapImportedCapability({ RestResource });
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
