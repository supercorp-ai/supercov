import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  acquireProjectLock,
  cleanCoverageStorage,
  isolatedWorkspacePath,
  prepareIsolatedWorkspace,
  recoverAbandonedRuns,
  removeIsolatedWorkspace,
  writeRunState,
} from "../../src/workspace";

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
    rmSync(directory, { recursive: true, force: true });
});

describe("isolated run workspaces", () => {
  it("copies source but never copies or mutates ordinary build output", () => {
    const root = project();
    mkdirSync(resolve(root, "node_modules/example"), { recursive: true });
    writeFileSync(resolve(root, "node_modules/example/index.js"), "module\n");
    const workspace = prepareIsolatedWorkspace(root, "2026-01-01T00-00-00-000Z");

    expect(readFileSync(resolve(workspace, "src/index.ts"), "utf8")).toBe(
      "export const value = 1;\n",
    );
    expect(existsSync(resolve(workspace, "dist"))).toBe(false);
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
    const stagedReport = resolve(
      root,
      ".supercov/work",
      runId,
      "report-publication/report.json.gz",
    );
    mkdirSync(resolve(stagedReport, ".."), { recursive: true });
    writeFileSync(stagedReport, "incomplete");
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
    expect(existsSync(stagedReport)).toBe(false);
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
    writeRunState(root, runId, {
      id: runId,
      pid: 2_147_483_647,
      root,
      workspace,
      startedAt: "2026-01-02T01:00:00.000Z",
      status: "reporting",
    });

    expect(recoverAbandonedRuns(root)).toEqual([runId]);
    expect(existsSync(workspace)).toBe(false);
    expect(existsSync(publishedRun)).toBe(true);
    const state = JSON.parse(
      readFileSync(resolve(root, ".supercov/work", runId, "state.json"), "utf8"),
    ) as { status: string };
    expect(state.status).toBe("complete");
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
});
