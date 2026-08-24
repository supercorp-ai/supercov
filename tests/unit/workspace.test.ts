import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  readdirSync,
  renameSync,
  rmSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { basename, dirname, relative, resolve, sep } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  acquireProjectLock,
  pruneCachedWorkspaceSources,
  cachedWorkspacePath,
  cleanCoverageStorage,
  isolatedWorkspacePath,
  prepareIsolatedWorkspace,
  prepareCachedWorkspace,
  pruneCoverageStorage,
  recoverCachedWorkspace,
  recoverAbandonedRuns,
  removeIsolatedWorkspace,
  removeStoredTreeDeferred,
  TRASH_DELETER_SCRIPT,
  writeRunState,
} from "../../src/workspace.ts";

const temporaryDirectories: string[] = [];

function project(): string {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-workspace-"));
  temporaryDirectories.push(root);
  mkdirSync(resolve(root, "src"));
  mkdirSync(resolve(root, "dist"));
  writeFileSync(resolve(root, "src/index.ts"), "export const value = 1;\n");
  writeFileSync(resolve(root, "dist/index.js"), "normal-build\n");
  writeFileSync(resolve(root, "package.json"), '{"name":"fixture"}\n');
  return root;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 20,
    });
});

describe("isolated run workspaces", () => {
  it("atomically moves owned data to deferred trash without touching arbitrary paths", () => {
    const root = project();
    const target = resolve(root, ".supercov/evidence/large-run");
    mkdirSync(target, { recursive: true });
    for (let index = 0; index < 100; index += 1)
      writeFileSync(resolve(target, `${index}.jsonl`), `${index}\n`);

    const trashed = removeStoredTreeDeferred(root, target);
    expect(existsSync(target)).toBe(false);
    expect(trashed && existsSync(trashed)).toBe(true);

    const outside = resolve(root, "src");
    expect(() => removeStoredTreeDeferred(root, outside)).toThrow(
      /outside Supercov-owned storage/,
    );
    expect(existsSync(resolve(outside, "index.ts"))).toBe(true);

    const trash = resolve(root, ".supercov/.trash");
    const deletion = spawnSync(
      process.execPath,
      ["-e", TRASH_DELETER_SCRIPT, trash],
      { encoding: "utf8" },
    );
    expect(deletion.status).toBe(0);
    expect(readdirSync(trash)).toEqual([]);
  });

  it("copies source but never copies or mutates ordinary build output", () => {
    const root = project();
    mkdirSync(resolve(root, ".cache/tool"), { recursive: true });
    writeFileSync(resolve(root, ".cache/tool/generated.js"), "cached\n");
    mkdirSync(resolve(root, "node_modules/example"), { recursive: true });
    writeFileSync(resolve(root, "node_modules/example/index.js"), "module\n");
    const workspace = prepareIsolatedWorkspace(root, "2026-01-01T00-00-00-000Z");

    expect(readFileSync(resolve(workspace, "src/index.ts"), "utf8")).toBe(
      "export const value = 1;\n",
    );
    expect(existsSync(resolve(workspace, "dist"))).toBe(false);
    expect(existsSync(resolve(workspace, ".cache"))).toBe(false);
    mkdirSync(resolve(workspace, "dist"));
    writeFileSync(resolve(workspace, "dist/index.js"), "instrumented-build\n");
    expect(readFileSync(resolve(root, "dist/index.js"), "utf8")).toBe(
      "normal-build\n",
    );
    expect(lstatSync(resolve(workspace, "node_modules")).isDirectory()).toBe(
      true,
    );
    expect(
      lstatSync(resolve(workspace, "node_modules/example")).isSymbolicLink(),
    ).toBe(true);
    expect(
      readFileSync(resolve(workspace, "node_modules/example/index.js"), "utf8"),
    ).toBe("module\n");

    removeIsolatedWorkspace(root, "2026-01-01T00-00-00-000Z");
    expect(existsSync(workspace)).toBe(false);
  });

  it("refreshes a stable isolated namespace for provider snapshot reuse", () => {
    const root = project();
    const first = prepareCachedWorkspace(root);
    expect(first).toBe(cachedWorkspacePath(root));
    writeFileSync(resolve(first, "stale.txt"), "stale");
    writeFileSync(resolve(root, "src/index.ts"), "export const value = 2;\n");
    const second = prepareCachedWorkspace(root);
    expect(second).toBe(first);
    expect(existsSync(resolve(second, "stale.txt"))).toBe(false);
    expect(readFileSync(resolve(second, "src/index.ts"), "utf8")).toContain(
      "value = 2",
    );
    expect(readFileSync(resolve(root, "dist/index.js"), "utf8")).toBe(
      "normal-build\n",
    );
    expect(
      readdirSync(dirname(second)).filter((entry) =>
        entry.startsWith(`.${basename(root)}.`),
      ),
    ).toEqual([]);
  });

  it("never traverses nested Supercov run stores", () => {
    const root = project();
    mkdirSync(resolve(root, "packages/example/.supercov/.cache"), {
      recursive: true,
    });
    writeFileSync(
      resolve(root, "packages/example/.supercov/.cache/stale.json"),
      "stale\n",
    );
    writeFileSync(resolve(root, "packages/example/source.ts"), "source\n");

    const workspace = prepareCachedWorkspace(root);
    expect(
      readFileSync(resolve(workspace, "packages/example/source.ts"), "utf8"),
    ).toBe("source\n");
    expect(existsSync(resolve(workspace, "packages/example/.supercov"))).toBe(
      false,
    );
  });

  it("carries only explicitly selected build artifacts into a refreshed snapshot", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    mkdirSync(resolve(workspace, "build"));
    mkdirSync(resolve(workspace, ".supercov"));
    writeFileSync(resolve(workspace, "build/index.js"), "instrumented\n");
    writeFileSync(resolve(workspace, ".supercov/manifest.json"), "manifest\n");
    writeFileSync(resolve(workspace, "unselected.txt"), "stale\n");

    prepareCachedWorkspace(root, {
      reusePaths: ["build", ".supercov/manifest.json"],
    });
    expect(readFileSync(resolve(workspace, "build/index.js"), "utf8")).toBe(
      "instrumented\n",
    );
    expect(
      readFileSync(resolve(workspace, ".supercov/manifest.json"), "utf8"),
    ).toBe("manifest\n");
    expect(existsSync(resolve(workspace, "unselected.txt"))).toBe(false);
  });

  it("refuses reuse paths and removals that leave the sandbox", () => {
    const root = project();
    mkdirSync(resolve(root, "node_modules/example"), { recursive: true });
    writeFileSync(resolve(root, "node_modules/example/index.js"), "module\n");
    const workspace = prepareCachedWorkspace(root);

    for (const requested of ["../outside", ".", "missing-path"]) {
      expect(() =>
        prepareCachedWorkspace(root, { reusePaths: [requested] }),
      ).toThrow(/Refusing to reuse unexpected build path/);
    }

    writeFileSync(resolve(workspace, "artifact.txt"), "artifact\n");
    symlinkSync(
      resolve(root, "package.json"),
      resolve(workspace, "linked-entry"),
    );
    expect(() =>
      prepareCachedWorkspace(root, { reusePaths: ["linked-entry"] }),
    ).toThrow(/Unsupported reusable build entry/);

    const copies: Array<[string, string]> = [];
    prepareCachedWorkspace(root, {
      reusePaths: ["artifact.txt"],
      copyFile: (source, destination) => {
        copies.push([source, destination]);
        copyFileSync(source, destination);
      },
    });
    expect(
      copies.filter(
        ([source]) => source === resolve(workspace, "artifact.txt"),
      ),
    ).toHaveLength(1);
    expect(readFileSync(resolve(workspace, "artifact.txt"), "utf8")).toBe(
      "artifact\n",
    );

    expect(() => removeIsolatedWorkspace(root, "..")).toThrow(
      /Refusing to remove unexpected workspace path/,
    );
    expect(() => removeIsolatedWorkspace(root, "")).toThrow(
      /Refusing to remove unexpected workspace path/,
    );
  });

  it("keeps every workspace path segment free of a leading dot", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);

    // `send` (and so express.static / serve-static / res.sendFile) answers 404
    // for any path containing a dot-prefixed segment, so an application under
    // test could not serve its own files from a dotted workspace.
    const segments = relative(root, workspace).split(sep);
    expect(segments.filter((segment) => segment.startsWith("."))).toEqual([]);
    expect(segments[0]).toBe("supercov");

    // The container hides itself from Git without the user editing anything.
    expect(
      readFileSync(resolve(root, "supercov/.gitignore"), "utf8"),
    ).toBe("*\n");
  });

  it("copies a project's own supercov directory but never its own workspace", () => {
    const root = project();
    // A project may legitimately own a directory called `supercov`; only ours
    // carries the marker, so only ours is skipped.
    mkdirSync(resolve(root, "packages/supercov/src"), { recursive: true });
    writeFileSync(
      resolve(root, "packages/supercov/src/index.ts"),
      "export const owned = true;\n",
    );

    const workspace = prepareCachedWorkspace(root);
    expect(
      readFileSync(
        resolve(workspace, "packages/supercov/src/index.ts"),
        "utf8",
      ),
    ).toBe("export const owned = true;\n");

    // A second refresh must not nest the previous workspace inside the new one.
    const refreshed = prepareCachedWorkspace(root);
    expect(existsSync(resolve(refreshed, "supercov"))).toBe(false);
    expect(readFileSync(resolve(refreshed, "src/index.ts"), "utf8")).toBe(
      "export const value = 1;\n",
    );
  });

  it("prunes copied sources from the cache but keeps reusable artifacts", () => {
    const root = project();
    mkdirSync(resolve(root, "node_modules/example"), { recursive: true });
    writeFileSync(resolve(root, "node_modules/example/index.js"), "module\n");
    writeFileSync(resolve(root, "src/index.test.ts"), "test copy\n");
    const workspace = prepareCachedWorkspace(root);

    mkdirSync(resolve(workspace, ".supercov"), { recursive: true });
    mkdirSync(resolve(workspace, "build"), { recursive: true });
    writeFileSync(resolve(workspace, "build/index.js"), "instrumented\n");
    writeFileSync(
      resolve(workspace, ".supercov/build-cache.json"),
      JSON.stringify({ artifactPaths: ["build", ".supercov/manifest.json"] }),
    );

    const removed = pruneCachedWorkspaceSources(root);
    expect(removed).toContain("src");
    expect(removed).toContain("package.json");
    expect(existsSync(resolve(workspace, "src"))).toBe(false);
    expect(existsSync(resolve(workspace, "node_modules/example"))).toBe(true);
    expect(readFileSync(resolve(workspace, "build/index.js"), "utf8")).toBe(
      "instrumented\n",
    );
    expect(
      existsSync(resolve(workspace, ".supercov/build-cache.json")),
    ).toBe(true);

    // A refresh after pruning restores a complete workspace.
    const refreshed = prepareCachedWorkspace(root);
    expect(readFileSync(resolve(refreshed, "src/index.ts"), "utf8")).toBe(
      "export const value = 1;\n",
    );

    expect(
      pruneCachedWorkspaceSources(
        resolve(root, "never-prepared-subdirectory"),
      ),
    ).toEqual([]);
  });

  it("recovers every interrupted cache publication boundary", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    const parent = dirname(workspace);
    const prefix = `.${basename(root)}`;

    const incompleteStaging = resolve(parent, `${prefix}.staging-interrupted`);
    mkdirSync(incompleteStaging);
    writeFileSync(resolve(incompleteStaging, "partial.txt"), "partial\n");
    expect(recoverCachedWorkspace(root)).toEqual({
      restoredPrevious: false,
      removedStaging: 1,
      removedPrevious: 0,
    });
    expect(existsSync(workspace)).toBe(true);
    expect(existsSync(incompleteStaging)).toBe(false);

    const previous = resolve(parent, `${prefix}.previous-interrupted`);
    const readyStaging = resolve(parent, `${prefix}.staging-ready`);
    renameSync(workspace, previous);
    mkdirSync(readyStaging);
    writeFileSync(resolve(readyStaging, "unpublished.txt"), "new\n");
    expect(recoverCachedWorkspace(root)).toEqual({
      restoredPrevious: true,
      removedStaging: 1,
      removedPrevious: 0,
    });
    expect(existsSync(resolve(workspace, "src/index.ts"))).toBe(true);
    expect(existsSync(readyStaging)).toBe(false);

    const obsoletePrevious = resolve(parent, `${prefix}.previous-obsolete`);
    mkdirSync(obsoletePrevious);
    writeFileSync(resolve(obsoletePrevious, "old.txt"), "old\n");
    expect(recoverCachedWorkspace(root)).toEqual({
      restoredPrevious: false,
      removedStaging: 0,
      removedPrevious: 1,
    });
    expect(existsSync(obsoletePrevious)).toBe(false);
  });

  it("preserves or recovers a complete generation when publication throws", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    writeFileSync(resolve(workspace, "generation.txt"), "old\n");

    expect(() =>
      prepareCachedWorkspace(root, {
        beforePublish() {
          throw new Error("injected before publication");
        },
      }),
    ).toThrow(/injected before publication/);
    expect(readFileSync(resolve(workspace, "generation.txt"), "utf8")).toBe(
      "old\n",
    );

    expect(() =>
      prepareCachedWorkspace(root, {
        afterPreviousMoved() {
          throw new Error("injected between renames");
        },
      }),
    ).toThrow(/injected between renames/);
    expect(readFileSync(resolve(workspace, "generation.txt"), "utf8")).toBe(
      "old\n",
    );

    writeFileSync(resolve(root, "src/index.ts"), "export const value = 3;\n");
    expect(() =>
      prepareCachedWorkspace(root, {
        afterPublished() {
          throw new Error("injected after publication");
        },
      }),
    ).toThrow(/injected after publication/);
    expect(readFileSync(resolve(workspace, "src/index.ts"), "utf8")).toContain(
      "value = 3",
    );
    expect(recoverCachedWorkspace(root).removedPrevious).toBe(1);
    expect(
      readdirSync(dirname(workspace)).filter((entry) =>
        entry.startsWith(`.${basename(root)}.`),
      ),
    ).toEqual([]);
  });

  it("restores the prior generation when the publication rename fails", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    writeFileSync(resolve(workspace, "generation.txt"), "old\n");
    let renames = 0;
    expect(() =>
      prepareCachedWorkspace(root, {
        rename(from, to) {
          renames += 1;
          if (renames === 2) {
            const failure = new Error("rename failed") as NodeJS.ErrnoException;
            failure.code = "EIO";
            throw failure;
          }
          renameSync(from, to);
        },
      }),
    ).toThrow(/rename failed/);
    expect(readFileSync(resolve(workspace, "generation.txt"), "utf8")).toBe(
      "old\n",
    );
    expect(recoverCachedWorkspace(root).removedPrevious).toBe(0);
  });

  it("falls back to an ordinary copy without changing publication semantics", () => {
    const root = project();
    let copied = 0;
    const workspace = prepareCachedWorkspace(root, {
      copyFile(from, to) {
        copied += 1;
        copyFileSync(from, to);
      },
    });
    expect(copied).toBeGreaterThan(0);
    expect(readFileSync(resolve(workspace, "src/index.ts"), "utf8")).toContain(
      "value = 1",
    );
  });

  it("preserves the previous generation after an ENOSPC copy failure", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    writeFileSync(resolve(workspace, "generation.txt"), "complete\n");
    let copied = 0;
    expect(() =>
      prepareCachedWorkspace(root, {
        copyFile(from, to) {
          copied += 1;
          if (copied === 2) {
            const failure = new Error("disk full") as NodeJS.ErrnoException;
            failure.code = "ENOSPC";
            throw failure;
          }
          copyFileSync(from, to);
        },
      }),
    ).toThrow(/disk full/);
    expect(readFileSync(resolve(workspace, "generation.txt"), "utf8")).toBe(
      "complete\n",
    );
    expect(
      readdirSync(dirname(workspace)).filter((entry) =>
        entry.startsWith(`.${basename(root)}.`),
      ),
    ).toEqual([]);
  });

  it("relocates an internal directory link into the isolated generation", () => {
    const root = project();
    symlinkSync(
      resolve(root, "src"),
      resolve(root, "linked-src"),
      process.platform === "win32" ? "junction" : "dir",
    );
    const workspace = prepareCachedWorkspace(root);
    expect(lstatSync(resolve(workspace, "linked-src")).isSymbolicLink()).toBe(
      true,
    );
    expect(realpathSync(resolve(workspace, "linked-src"))).toBe(
      realpathSync(resolve(workspace, "src")),
    );
  });

  it("keeps internal symlinks isolated and rejects links outside the project", () => {
    if (process.platform === "win32") return;
    const root = project();
    symlinkSync(
      resolve(root, "src/index.ts"),
      resolve(root, "src/absolute-link.ts"),
    );
    const workspace = prepareCachedWorkspace(root);
    const isolatedLink = resolve(workspace, "src/absolute-link.ts");
    expect(readlinkSync(isolatedLink)).toBe("index.ts");
    expect(realpathSync(isolatedLink)).toBe(
      realpathSync(resolve(workspace, "src/index.ts")),
    );
    writeFileSync(resolve(workspace, "src/index.ts"), "isolated\n");
    expect(readFileSync(resolve(root, "src/index.ts"), "utf8")).toContain(
      "value = 1",
    );

    const external = mkdtempSync(resolve(tmpdir(), "supercov-external-"));
    temporaryDirectories.push(external);
    writeFileSync(resolve(external, "shared.ts"), "external\n");
    symlinkSync(
      resolve(external, "shared.ts"),
      resolve(root, "src/external-link.ts"),
    );
    expect(() => prepareCachedWorkspace(root)).toThrow(
      /symlink outside the isolated project/,
    );
    expect(readFileSync(resolve(workspace, "src/index.ts"), "utf8")).toBe(
      "isolated\n",
    );
    expect(
      readdirSync(dirname(workspace)).filter((entry) =>
        entry.startsWith(`.${basename(root)}.`),
      ),
    ).toEqual([]);
  });

  it("rejects concurrent runs and recovers a stale project lock", () => {
    const root = project();
    const first = acquireProjectLock(root, "first");
    expect(() => acquireProjectLock(root, "second")).toThrow(
      /first.*already active/,
    );
    first.release();

    mkdirSync(resolve(root, ".supercov/locks"), { recursive: true });
    writeFileSync(resolve(root, ".supercov/locks/active.json"), "");
    expect(() => acquireProjectLock(root, "racing")).toThrow(/acquiring/);
    const old = new Date(Date.now() - 60_000);
    utimesSync(resolve(root, ".supercov/locks/active.json"), old, old);
    const afterIncomplete = acquireProjectLock(root, "after-incomplete");
    afterIncomplete.release();

    writeFileSync(
      resolve(root, ".supercov/locks/active.json"),
      '{"runId":"dead","pid":2147483647}\n',
    );
    const recovered = acquireProjectLock(root, "replacement");
    expect(readFileSync(recovered.path, "utf8")).toContain('"replacement"');
    recovered.release();
    expect(existsSync(recovered.path)).toBe(false);
  });

  it("marks killed runs abandoned and deletes only their disposable copy", () => {
    const root = project();
    const runId = "2026-01-02T00-00-00-000Z";
    const workspace = prepareIsolatedWorkspace(root, runId);
    const stagedRun = resolve(
      root,
      ".supercov/work",
      runId,
      "run-publication/evidence.raw.gz",
    );
    mkdirSync(resolve(stagedRun, ".."), { recursive: true });
    writeFileSync(stagedRun, "incomplete");
    const orphanEvidence = resolve(root, ".supercov/evidence", runId, "hit.json");
    mkdirSync(resolve(orphanEvidence, ".."), { recursive: true });
    writeFileSync(orphanEvidence, "partial");
    writeRunState(root, runId, {
      id: runId,
      pid: 2_147_483_647,
      root,
      // Recovery must derive its deletion target, not trust persisted state.
      workspace: resolve(root, "dist"),
      startedAt: "2026-01-02T00:00:00.000Z",
      status: "testing",
    });

    expect(recoverAbandonedRuns(root)).toEqual([runId]);
    expect(existsSync(workspace)).toBe(false);
    expect(existsSync(stagedRun)).toBe(false);
    expect(existsSync(orphanEvidence)).toBe(false);
    const state = JSON.parse(
      readFileSync(resolve(root, ".supercov/work", runId, "state.json"), "utf8"),
    ) as { status: string; error: string };
    expect(state.status).toBe("abandoned");
    expect(state.error).toMatch(/exited without cleanup/);
    expect(readFileSync(resolve(root, "dist/index.js"), "utf8")).toBe(
      "normal-build\n",
    );
  });

  it("recovers a completely published run killed before its terminal state write", () => {
    const root = project();
    const runId = "2026-01-02T01-00-00-000Z";
    const workspace = prepareIsolatedWorkspace(root, runId);
    const publishedRun = resolve(root, ".supercov/runs", runId, "run.json");
    mkdirSync(resolve(publishedRun, ".."), { recursive: true });
    writeFileSync(publishedRun, `${JSON.stringify({ id: runId })}\n`);
    writeFileSync(resolve(publishedRun, "../evidence.raw.gz"), "evidence");
    const looseEvidence = resolve(root, ".supercov/evidence", runId, "hit.json");
    mkdirSync(resolve(looseEvidence, ".."), { recursive: true });
    writeFileSync(looseEvidence, "loose");
    writeRunState(root, runId, {
      id: runId,
      pid: 2_147_483_647,
      root,
      workspace,
      startedAt: "2026-01-02T01:00:00.000Z",
      status: "publishing",
    });

    expect(recoverAbandonedRuns(root)).toEqual([runId]);
    expect(existsSync(workspace)).toBe(false);
    expect(existsSync(publishedRun)).toBe(true);
    expect(existsSync(looseEvidence)).toBe(false);
    expect(existsSync(resolve(root, ".supercov/work", runId))).toBe(false);
  });

  it("retains the newest runs deterministically and supports dry runs", () => {
    const root = project();
    const ids = [
      "2026-01-01T00-00-00-000Z",
      "2026-01-02T00-00-00-000Z",
      "2026-01-03T00-00-00-000Z",
    ];
    for (const id of ids) {
      mkdirSync(resolve(root, ".supercov/runs", id), { recursive: true });
      mkdirSync(resolve(root, ".supercov/evidence", id), { recursive: true });
      const workspace = isolatedWorkspacePath(root, id);
      mkdirSync(workspace, { recursive: true });
      writeRunState(root, id, {
        id,
        pid: process.pid,
        root,
        workspace,
        startedAt: id,
        status: "complete",
      });
    }
    const activeId = "2025-12-31T00-00-00-000Z";
    const activeWorkspace = isolatedWorkspacePath(root, activeId);
    mkdirSync(activeWorkspace, { recursive: true });
    writeRunState(root, activeId, {
      id: activeId,
      pid: process.pid,
      root,
      workspace: activeWorkspace,
      startedAt: activeId,
      status: "testing",
    });

    const preview = cleanCoverageStorage(root, { keep: 1, dryRun: true });
    expect(preview.removedRuns).toEqual(ids.slice(0, 2).reverse());
    expect(ids.every((id) => existsSync(resolve(root, ".supercov/runs", id)))).toBe(
      true,
    );

    const cleaned = cleanCoverageStorage(root, { keep: 1, dryRun: false });
    expect(cleaned.removedRuns).toEqual(preview.removedRuns);
    expect(existsSync(resolve(root, ".supercov/runs", ids[2]!))).toBe(true);
    expect(existsSync(resolve(root, ".supercov/runs", ids[1]!))).toBe(false);
    expect(existsSync(isolatedWorkspacePath(root, ids[2]!))).toBe(false);
    expect(existsSync(activeWorkspace)).toBe(true);
  });

  it("never removes the stable cache while the project lock is live", () => {
    const root = project();
    const workspace = prepareCachedWorkspace(root);
    const lock = acquireProjectLock(root, "active-clean-test");
    try {
      expect(() =>
        cleanCoverageStorage(root, { keep: 0, dryRun: false }),
      ).toThrow(/active-clean-test.*already active/);
      expect(existsSync(workspace)).toBe(true);
    } finally {
      lock.release();
    }

    const inactive = cleanCoverageStorage(root, { keep: 0, dryRun: false });
    expect(inactive.removedBuildCache).toBe(true);
    expect(existsSync(workspace)).toBe(false);
  });

  it("cleans obsolete dotted cache layouts but prune preserves them", () => {
    const root = project();
    const oldCaches = [
      resolve(root, ".supercov/.cache/instrumented-workspace"),
      resolve(root, ".supercov/cache/instrumented-workspace"),
    ];
    for (const cache of oldCaches) {
      mkdirSync(cache, { recursive: true });
      writeFileSync(resolve(cache, "stale.js"), "stale\n");
    }

    const pruned = pruneCoverageStorage(root, { keep: 0, dryRun: false });
    expect(pruned.removedBuildCache).toBe(false);
    expect(oldCaches.every((cache) => existsSync(cache))).toBe(true);

    const cleaned = cleanCoverageStorage(root, { keep: 0, dryRun: false });
    expect(cleaned.removedBuildCache).toBe(true);
    expect(oldCaches.every((cache) => !existsSync(cache))).toBe(true);
  });

  it("prunes explicit history and terminal work without removing the shared cache", () => {
    const root = project();
    const cache = prepareCachedWorkspace(root);
    const ids = [
      "2026-01-01T00-00-00-000Z",
      "2026-01-02T00-00-00-000Z",
    ];
    for (const id of ids) {
      mkdirSync(resolve(root, ".supercov/runs", id), { recursive: true });
      writeFileSync(
        resolve(root, ".supercov/runs", id, "run.json"),
        `${JSON.stringify({ id })}\n`,
      );
      mkdirSync(resolve(root, ".supercov/evidence", id), { recursive: true });
      writeFileSync(resolve(root, ".supercov/evidence", id, "hit.json"), "{}");
      writeRunState(root, id, {
        id,
        pid: process.pid,
        root,
        workspace: cache,
        startedAt: id,
        status: "complete",
      });
    }

    const result = pruneCoverageStorage(root, { keep: 1, dryRun: false });
    expect(result.removedRuns).toEqual([ids[0]]);
    expect(result.removedWorkspaces).toEqual(ids.slice().reverse());
    expect(result.removedEvidence).toEqual([ids[0]]);
    expect(result.removedBuildCache).toBe(false);
    expect(existsSync(cache)).toBe(true);
    expect(existsSync(resolve(root, ".supercov/runs", ids[1]!))).toBe(true);
    expect(existsSync(resolve(root, ".supercov/evidence", ids[1]!))).toBe(true);
  });
});
