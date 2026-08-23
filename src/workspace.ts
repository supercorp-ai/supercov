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
  realpathSync,
  rmSync,
  symlinkSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { randomUUID } from "node:crypto";
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { atomicRenameSync, atomicWriteFileSync } from "./atomic.ts";

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

function cacheTransactionPrefix(
  root: string,
  kind: "staging" | "previous",
): string {
  return `.${basename(root)}.${kind}-`;
}

function cacheTransactionPath(
  root: string,
  kind: "staging" | "previous",
): string {
  const workspace = cachedWorkspacePath(root);
  return resolve(
    dirname(workspace),
    `${cacheTransactionPrefix(root, kind)}${process.pid}-${randomUUID()}`,
  );
}

export interface CacheRecoveryResult {
  restoredPrevious: boolean;
  removedStaging: number;
  removedPrevious: number;
}

/**
 * Recover the stable cache at transaction boundaries that SIGKILL or a host
 * crash can interrupt. A staging tree is never live. A previous tree is
 * restored only when the stable name is absent; otherwise it is obsolete.
 */
export function recoverCachedWorkspace(root: string): CacheRecoveryResult {
  const workspace = cachedWorkspacePath(root);
  const parent = dirname(workspace);
  if (!existsSync(parent))
    return { restoredPrevious: false, removedStaging: 0, removedPrevious: 0 };

  const entries = readdirSync(parent, { withFileTypes: true });
  const stagingPrefix = cacheTransactionPrefix(root, "staging");
  const previousPrefix = cacheTransactionPrefix(root, "previous");
  const staging = entries
    .filter((entry) => entry.name.startsWith(stagingPrefix))
    .map((entry) => resolve(parent, entry.name));
  const previous = entries
    .filter(
      (entry) => entry.name.startsWith(previousPrefix) && entry.isDirectory(),
    )
    .map((entry) => resolve(parent, entry.name))
    .sort((left, right) => lstatSync(right).mtimeMs - lstatSync(left).mtimeMs);
  const invalidPrevious = entries
    .filter(
      (entry) => entry.name.startsWith(previousPrefix) && !entry.isDirectory(),
    )
    .map((entry) => resolve(parent, entry.name));

  let restoredPrevious = false;
  if (!existsSync(workspace) && previous[0]) {
    atomicRenameSync(previous[0], workspace);
    previous.shift();
    restoredPrevious = true;
  }

  for (const path of staging) rmSync(path, { recursive: true, force: true });
  for (const path of previous) rmSync(path, { recursive: true, force: true });
  for (const path of invalidPrevious)
    rmSync(path, { recursive: true, force: true });
  return {
    restoredPrevious,
    removedStaging: staging.length,
    removedPrevious: previous.length + invalidPrevious.length,
  };
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

function inside(root: string, path: string): boolean {
  const local = relative(root, path);
  return (
    local === "" ||
    (!local.startsWith(`..${sep}`) && local !== ".." && !isAbsolute(local))
  );
}

function copyTree(
  source: string,
  destination: string,
  root = false,
  sourceRoot = source,
  destinationRoot = destination,
): void {
  mkdirSync(destination, { recursive: true });
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    if (root && ROOT_EXCLUSIONS.has(entry.name)) continue;
    const from = resolve(source, entry.name);
    const to = resolve(destination, entry.name);
    const stat = lstatSync(from);
    if (stat.isDirectory())
      copyTree(from, to, false, sourceRoot, destinationRoot);
    else if (stat.isSymbolicLink()) {
      const link = readlinkSync(from);
      const lexicalTarget = isAbsolute(link)
        ? resolve(link)
        : resolve(dirname(from), link);
      let finalTarget: string | undefined;
      try {
        finalTarget = realpathSync(from);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      }
      if (
        !inside(sourceRoot, lexicalTarget) ||
        (finalTarget && !inside(realpathSync(sourceRoot), finalTarget))
      ) {
        throw new Error(
          `Refusing to preserve symlink outside the isolated project: ${relative(sourceRoot, from)} -> ${link}`,
        );
      }
      const isolatedLink = isAbsolute(link)
        ? relative(
            dirname(to),
            resolve(destinationRoot, relative(sourceRoot, lexicalTarget)),
          )
        : link;
      symlinkSync(
        isolatedLink,
        to,
        process.platform === "win32"
          ? statSync(from).isDirectory()
            ? "junction"
            : "file"
          : undefined,
      );
    } else if (stat.isFile())
      copyFileSync(from, to, constants.COPYFILE_FICLONE);
    else
      throw new Error(
        `Unsupported filesystem entry in isolated project: ${relative(sourceRoot, from)}`,
      );
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

interface CachePreparationHooks {
  /** Internal fault-injection seam used by the crash-boundary regression. */
  beforePublish?: (staging: string) => void;
  /** Internal fault-injection seam used by the crash-boundary regression. */
  afterPreviousMoved?: (previous: string) => void;
  /** Internal fault-injection seam used by the crash-boundary regression. */
  afterPublished?: (workspace: string) => void;
}

/** Refresh the stable, disposable instrumented-build namespace. */
export function prepareCachedWorkspace(
  root: string,
  hooks: CachePreparationHooks = {},
): string {
  const workspace = cachedWorkspacePath(root);
  recoverCachedWorkspace(root);
  const staging = cacheTransactionPath(root, "staging");
  const previous = cacheTransactionPath(root, "previous");
  let movedPrevious = false;
  try {
    copyTree(root, staging, true);
    const nodeModules = resolve(root, "node_modules");
    if (existsSync(nodeModules)) {
      const isolatedNodeModules = resolve(staging, "node_modules");
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
    hooks.beforePublish?.(staging);

    // Keep the last complete cache available for the whole refresh. These two
    // same-filesystem renames are the only publication boundary. Recovery
    // restores `previous` if the process is killed in the narrow gap.
    if (existsSync(workspace)) {
      atomicRenameSync(workspace, previous);
      movedPrevious = true;
      hooks.afterPreviousMoved?.(previous);
    }
    atomicRenameSync(staging, workspace);
    hooks.afterPublished?.(workspace);
  } catch (error) {
    if (movedPrevious && !existsSync(workspace) && existsSync(previous)) {
      try {
        atomicRenameSync(previous, workspace);
      } catch {
        // Leave the previous tree for deterministic recovery next invocation.
      }
    }
    throw error;
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }

  // Failure to remove an obsolete previous generation must not invalidate the
  // newly published cache. The next invocation and `supercov clean` both
  // remove it deterministically.
  try {
    rmSync(previous, { recursive: true, force: true });
  } catch {
    // Deliberately retained for recoverCachedWorkspace().
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
function cleanCoverageStorageLocked(
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

/** Cleanup is itself a project-wide transaction, so it cannot race a run. */
export function cleanCoverageStorage(
  root: string,
  options: CleanupOptions,
): CleanupResult {
  const lock = acquireProjectLock(
    root,
    `clean-${process.pid}-${randomUUID()}`,
  );
  try {
    return cleanCoverageStorageLocked(root, options);
  } finally {
    lock.release();
  }
}
