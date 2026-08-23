import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  discoverWorkspaceMapping,
  guestCoverageEnvironment,
  scopeCapabilityCache,
  wrapCapabilityObject,
} from "../../src/launchSupervisor";

describe("generic launch supervision", () => {
  it("discovers a mounted isolated workspace without provider knowledge", () => {
    expect(
      discoverWorkspaceMapping(
        {
          mounts: [
            { source: "/tmp/unrelated", target: "/data" },
            { hostPath: "/tmp/run/project", guestPath: "/workspace" },
          ],
        },
        "/tmp/run/project",
      ),
    ).toEqual({
      hostRoot: resolve("/tmp/run/project"),
      guestRoot: resolve("/workspace"),
    });
  });

  it("maps a project nested inside a mounted parent", () => {
    expect(
      discoverWorkspaceMapping(
        { mounts: [{ source: "/tmp/run", target: "/sandbox" }] },
        "/tmp/run/project",
      ),
    ).toEqual({
      hostRoot: resolve("/tmp/run/project"),
      guestRoot: resolve("/sandbox/project"),
    });
  });

  it("scopes existing generic cache identities without inventing options", () => {
    const options = {
      warmupTag: "application-dependencies",
      nested: { snapshotKey: "base", ordinaryTag: "unchanged" },
      mounts: [{ hostPath: "/repo", guestPath: "/workspace" }],
    };
    const scoped = scopeCapabilityCache(options, "0123456789abcdef0123456789");
    expect(scoped.changed).toEqual(["warmupTag", "nested.snapshotKey"]);
    expect(scoped.value).toMatchObject({
      warmupTag: "application-dependencies-supercov-0123456789abcdef0123",
      nested: {
        snapshotKey: "base-supercov-0123456789abcdef0123",
        ordinaryTag: "unchanged",
      },
    });
    expect(options.warmupTag).toBe("application-dependencies");
  });

  it("translates only Supercov paths and preserves a remote environment", () => {
    const environment = guestCoverageEnvironment(
      { hostRoot: "/tmp/run/project", guestRoot: "/workspace" },
      {
        SUPERCOV_PROJECT_ROOT: "/tmp/run/project",
        SUPERCOV_MANIFEST: "/tmp/run/project/.supercov/manifest.json",
        SUPERCOV_EVIDENCE_DIR: ".supercov/evidence/run",
        SECRET_FROM_HOST: "must-not-propagate",
      },
      { DATABASE_URL: "isolated", NODE_OPTIONS: "--trace-warnings" },
    );
    expect(environment).toMatchObject({
      DATABASE_URL: "isolated",
      SUPERCOV_PROJECT_ROOT: "/workspace",
      SUPERCOV_MANIFEST: "/workspace/.supercov/manifest.json",
      SUPERCOV_EVIDENCE_DIR: ".supercov/evidence/run",
      SUPERCOV_CJS_INTERCEPT: "1",
    });
    expect(environment.SECRET_FROM_HOST).toBeUndefined();
    expect(environment.NODE_OPTIONS).toContain("--trace-warnings");
    expect(environment.NODE_OPTIONS).toContain(
      "--import=file:///workspace/.supercov/register.mjs",
    );
  });

  it("follows opaque capability objects and injects argv launches", async () => {
    const received: Record<string, unknown>[] = [];
    const machine = {
      async exec(options: Record<string, unknown>) {
        received.push(options);
        return { exitCode: 0 };
      },
    };
    const pool = {
      async acquire() {
        return machine;
      },
    };
    const image = {
      createPool() {
        return pool;
      },
    };
    const wrapped = wrapCapabilityObject(image, {
      hostRoot: "/tmp/run/project",
      guestRoot: "/workspace",
    }) as typeof image;
    const acquired = await wrapped.createPool().acquire();
    await acquired.exec({
      argv: ["npm", "test"],
      env: { DATABASE_URL: "isolated" },
    });
    await acquired.exec({
      argv: ["npm", "test:second"],
      env: { DATABASE_URL: "isolated" },
    });
    expect(received[0]).toMatchObject({
      argv: ["npm", "test"],
      env: {
        DATABASE_URL: "isolated",
        SUPERCOV_PROJECT_ROOT: "/workspace",
        SUPERCOV_CJS_INTERCEPT: "1",
      },
    });
    const firstShard = (received[0]?.env as NodeJS.ProcessEnv)
      .SUPERCOV_EXECUTION_LOG_SHARD;
    const secondShard = (received[1]?.env as NodeJS.ProcessEnv)
      .SUPERCOV_EXECUTION_LOG_SHARD;
    expect(firstShard).toMatch(/^\d+-\d+$/);
    expect(secondShard).toMatch(/^\d+-\d+$/);
    expect(secondShard).not.toBe(firstShard);
  });
});
