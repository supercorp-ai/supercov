import {
  copyFileSync,
  constants,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  closeSync,
  fsyncSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, relative, resolve, sep } from "node:path";
import { atomicWriteFileSync } from "./atomic.ts";

export type RunStateStatus =
  | "preparing"
  | "building"
  | "testing"
  | "reporting"
  | "complete"
  | "failed"
  | "interrupted"
  | "abandoned";

export interface RunState {
  id: string;
  pid: number;
  root: string;
  workspace: string;
  startedAt: string;
  updatedAt: string;
  status: RunStateStatus;
  signal?: NodeJS.Signals;
  error?: string;
}

const TERMINAL = new Set<RunStateStatus>([
  "complete",
  "failed",
  "interrupted",
  "abandoned",
]);
const ROOT_EXCLUSIONS = new Set([
  ".git",
  ".supercov",
  ".mcdc-pool",
  "node_modules",
  "build",
  "dist",
  ".next",
  ".nuxt",
  ".output",
  "coverage",
  "playwright-report",
  "test-results",
]);

function processExists(pid: number): boolean {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

function readJson<T>(path: string): T | undefined {
  try {
    return JSON.parse(readFileSync(path, "utf8")) as T;
  } catch {
    return undefined;
  }
}

function statePath(root: string, runId: string): string {
  return resolve(root, ".supercov/work", runId, "state.json");
}

export function isolatedWorkspacePath(root: string, runId: string): string {
  return resolve(root, ".supercov/work", runId, basename(root));
}

/**
 * Stable isolated namespace used as a live mount by VM/container runners.
 * Its path must remain stable so providers whose snapshot key includes mount
 * paths can reuse an instrumented snapshot across equivalent coverage runs.
 * The project lock makes refreshes single-writer.
 */
export function cachedWorkspacePath(root: string): string {
  return resolve(root, ".supercov/cache/instrumented-workspace", basename(root));
}

export function writeRunState(
  root: string,
  runId: string,
  state: Omit<RunState, "updatedAt">,
): RunState {
  const complete = { ...state, updatedAt: new Date().toISOString() };
  atomicWriteFileSync(
    statePath(root, runId),
    `${JSON.stringify(complete, null, 2)}\n`,
  );
  return complete;
}

export function updateRunState(
  root: string,
  runId: string,
  update: Partial<Omit<RunState, "id" | "root" | "workspace" | "startedAt">>,
): RunState {
  const current = readJson<RunState>(statePath(root, runId));
  if (!current) throw new Error(`Run state is missing for ${runId}`);
  const next: RunState = {
    ...current,
    ...update,
    updatedAt: new Date().toISOString(),
  };
  atomicWriteFileSync(
    statePath(root, runId),
    `${JSON.stringify(next, null, 2)}\n`,
  );
  return next;
}

/** Mark dead runs abandoned and remove only their isolated workspace copy. */
export function recoverAbandonedRuns(root: string): string[] {
  const workRoot = resolve(root, ".supercov/work");
  if (!existsSync(workRoot)) return [];
  const recovered: string[] = [];
  for (const entry of readdirSync(workRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const path = statePath(root, entry.name);
    const state = readJson<RunState>(path);
    if (!state || TERMINAL.has(state.status) || processExists(state.pid))
      continue;
    // Never trust a persisted path as a deletion target. A partially written
    // or manually edited state file cannot widen cleanup beyond this run's
    // deterministic workspace namespace.
    rmSync(isolatedWorkspacePath(root, entry.name), {
      recursive: true,
      force: true,
    });
    rmSync(resolve(root, ".supercov/work", entry.name, "report-publication"), {
      recursive: true,
      force: true,
    });
    const publishedRun = readJson<{ id?: string }>(
      resolve(root, ".supercov/runs", entry.name, "run.json"),
    );
    if (publishedRun?.id === entry.name) {
      // The report directory is published by one atomic rename before the run
      // state flips terminal. A kill in that tiny window must recover the
      // already complete run rather than discard or mislabel it.
      updateRunState(root, entry.name, { status: "complete" });
    } else {
      // Evidence moved out of the disposable workspace before report
      // generation is not a visible run. Remove that orphan on recovery so a
      // hard kill cannot accumulate partial run data indefinitely.
      rmSync(resolve(root, ".supercov/evidence", entry.name), {
        recursive: true,
        force: true,
      });
      updateRunState(root, entry.name, {
        status: "abandoned",
        error: `Recovered after process ${state.pid} exited without cleanup`,
      });
    }
    recovered.push(entry.name);
  }
  return recovered.sort();
}

export interface ProjectLock {
  path: string;
  release: () => void;
}

export function acquireProjectLock(root: string, runId: string): ProjectLock {
  const lockPath = resolve(root, ".supercov/locks/active.json");
  mkdirSync(dirname(lockPath), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    let descriptor: number | undefined;
    try {
      descriptor = openSync(lockPath, "wx", 0o600);
      const payload = `${JSON.stringify({ runId, pid: process.pid, startedAt: new Date().toISOString() }, null, 2)}\n`;
      writeFileSync(descriptor, payload);
      fsyncSync(descriptor);
      closeSync(descriptor);
      descriptor = undefined;
      let released = false;
      return {
        path: lockPath,
        release() {
          if (released) return;
          released = true;
          const owner = readJson<{ runId?: string; pid?: number }>(lockPath);
          if (owner?.runId === runId && owner.pid === process.pid)
            rmSync(lockPath, { force: true });
        },
      };
    } catch (error) {
      if (descriptor !== undefined) closeSync(descriptor);
      const failure = error as NodeJS.ErrnoException;
      if (failure.code !== "EEXIST") throw error;
      const owner = readJson<{ runId?: string; pid?: number }>(lockPath);
      if (owner?.pid && processExists(owner.pid)) {
        throw new Error(
          `Coverage run ${owner.runId ?? "unknown"} is already active in this project (pid ${owner.pid})`,
        );
      }
      if (!owner) {
        const ageMs = Date.now() - statSync(lockPath).mtimeMs;
        if (ageMs < 30_000)
          throw new Error("A coverage run is currently acquiring the project lock");
      }
      rmSync(lockPath, { force: true });
    }
  }
  throw new Error("Could not acquire the Supercov project lock");
}

function copyTree(source: string, destination: string, root = false): void {
  mkdirSync(destination, { recursive: true });
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    if (root && ROOT_EXCLUSIONS.has(entry.name)) continue;
    const from = resolve(source, entry.name);
    const to = resolve(destination, entry.name);
    const stat = lstatSync(from);
    if (stat.isDirectory()) copyTree(from, to);
    else if (stat.isSymbolicLink())
      symlinkSync(readlinkSync(from), to, process.platform === "win32" ? "junction" : undefined);
    else if (stat.isFile()) copyFileSync(from, to, constants.COPYFILE_FICLONE);
  }
}

/** Create a copy-on-write project snapshot whose build outputs are disposable. */
export function prepareIsolatedWorkspace(root: string, runId: string): string {
  const workspace = isolatedWorkspacePath(root, runId);
  rmSync(workspace, { recursive: true, force: true });
  copyTree(root, workspace, true);
  const nodeModules = resolve(root, "node_modules");
  if (existsSync(nodeModules)) {
    // Keep the node_modules mount point itself as a real directory. VM-based
    // runners commonly layer a Linux dependency cache onto the mounted
    // workspace's node_modules path;
    // making the mount point an external symlink causes virtio-fs's safe
    // `opaque` policy to reject LOOKUP with EACCES before the nested mount can
    // be attached. Per-entry links still give host-side builds zero-copy
    // access to the user's installed dependencies and are hidden by the VM's
    // nested mount.
    const isolatedNodeModules = resolve(workspace, "node_modules");
    mkdirSync(isolatedNodeModules, { recursive: true });
    for (const entry of readdirSync(nodeModules, { withFileTypes: true })) {
      const target = resolve(nodeModules, entry.name);
      symlinkSync(
        target,
        resolve(isolatedNodeModules, entry.name),
        process.platform === "win32"
          ? entry.isDirectory()
            ? "junction"
            : "file"
          : undefined,
      );
    }
  }
  return workspace;
}

/** Refresh the stable, disposable instrumented-build namespace. */
export function prepareCachedWorkspace(root: string): string {
  const workspace = cachedWorkspacePath(root);
  rmSync(workspace, { recursive: true, force: true });
  copyTree(root, workspace, true);
  const nodeModules = resolve(root, "node_modules");
  if (existsSync(nodeModules)) {
    const isolatedNodeModules = resolve(workspace, "node_modules");
    mkdirSync(isolatedNodeModules, { recursive: true });
    for (const entry of readdirSync(nodeModules, { withFileTypes: true })) {
      symlinkSync(
        resolve(nodeModules, entry.name),
        resolve(isolatedNodeModules, entry.name),
        process.platform === "win32"
          ? entry.isDirectory()
            ? "junction"
            : "file"
          : undefined,
      );
    }
  }
  return workspace;
}

export function removeIsolatedWorkspace(root: string, runId: string): void {
  const workspace = isolatedWorkspacePath(root, runId);
  const relativePath = relative(resolve(root, ".supercov/work"), workspace);
  if (relativePath.startsWith("..") || relativePath.split(sep).length < 2) {
    throw new Error(`Refusing to remove unexpected workspace path: ${workspace}`);
  }
  rmSync(workspace, { recursive: true, force: true });
}

export interface CleanupOptions {
  keep: number;
  dryRun: boolean;
}

export interface CleanupResult {
  removedRuns: string[];
  removedWorkspaces: string[];
  removedBuildCache: boolean;
}

/** Deterministic retention: IDs sort newest-first because run IDs are UTC. */
export function cleanCoverageStorage(
  root: string,
  options: CleanupOptions,
): CleanupResult {
  recoverAbandonedRuns(root);
  const bases = ["runs", "work", "evidence"].map((name) =>
    resolve(root, ".supercov", name),
  );
  const ids = new Set<string>();
  for (const base of bases) {
    if (!existsSync(base)) continue;
    for (const entry of readdirSync(base, { withFileTypes: true }))
      if (entry.isDirectory()) ids.add(entry.name);
  }
  const ordered = [...ids].sort((left, right) => right.localeCompare(left));
  const active = new Set(
    ordered.filter((id) => {
      const state = readJson<RunState>(resolve(root, ".supercov/work", id, "state.json"));
      return Boolean(state && !TERMINAL.has(state.status));
    }),
  );
  const retained = new Set(
    ordered.filter((id) => !active.has(id)).slice(0, Math.max(0, options.keep)),
  );
  const removedRuns: string[] = [];
  const removedWorkspaces: string[] = [];
  for (const id of ordered) {
    const work = resolve(root, ".supercov/work", id);
    const state = readJson<RunState>(resolve(work, "state.json"));
    const workspace = isolatedWorkspacePath(root, id);
    if (active.has(id)) continue;
    if (existsSync(workspace) && (!state || TERMINAL.has(state.status))) {
      removedWorkspaces.push(id);
      if (!options.dryRun) rmSync(workspace, { recursive: true, force: true });
    }
    if (retained.has(id)) continue;
    removedRuns.push(id);
    if (!options.dryRun) {
      for (const base of bases)
        rmSync(resolve(base, id), { recursive: true, force: true });
    }
  }
  const buildCache = resolve(root, ".supercov/cache/instrumented-workspace");
  const removedBuildCache = active.size === 0 && existsSync(buildCache);
  if (removedBuildCache && !options.dryRun)
    rmSync(buildCache, { recursive: true, force: true });
  return { removedRuns, removedWorkspaces, removedBuildCache };
}
