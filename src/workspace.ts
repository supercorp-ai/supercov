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
  renameSync,
  rmSync,
  symlinkSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { atomicRenameSync, atomicWriteFileSync } from "./atomic.ts";

export const RUN_STORE_CONTRACT_VERSION = 1;

export type RunStateStatus =
  | "preparing"
  | "building"
  | "testing"
  | "publishing"
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
  ".cache",
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
const NESTED_SUPERCOV_EXCLUSIONS = new Set([".supercov", ".mcdc-pool"]);

const TRASH_DIRECTORY = ".supercov/.trash";

/**
 * Unlinking a large tree (workspace copies, per-attempt evidence) can take
 * minutes of uninterruptible I/O — observed at 26+ minutes on a real project.
 * Removal must never block the command: rename the tree into the store's
 * trash (atomic on the same filesystem, so the path disappears instantly) and
 * let a detached deleter unlink it after this process has already returned.
 * A crash between rename and unlink just leaves the entry in the trash, and
 * every later invocation sweeps whatever it finds there.
 */
export function removeStoredTreeDeferred(
  root: string,
  target: string,
): string | undefined {
  const resolvedRoot = resolve(root);
  const resolvedTarget = resolve(target);
  if (!existsSync(resolvedTarget)) return undefined;
  const trash = resolve(root, TRASH_DIRECTORY);
  const store = resolve(resolvedRoot, ".supercov");
  const container = workspaceContainerPath(resolvedRoot);
  const inStore =
    resolvedTarget !== store &&
    inside(store, resolvedTarget) &&
    !inside(trash, resolvedTarget);
  const inWorkspaceContainer =
    isWorkspaceContainer(container) && inside(container, resolvedTarget);
  if (!inStore && !inWorkspaceContainer)
    throw new Error(
      `Refusing to defer removal outside Supercov-owned storage: ${resolvedTarget}`,
    );
  mkdirSync(trash, { recursive: true });
  const destination = resolve(trash, `${process.pid}-${randomUUID()}`);
  try {
    renameSync(resolvedTarget, destination);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code ?? "unknown";
    throw new Error(
      `Could not move Supercov data into deferred trash (${code}): ${resolvedTarget}`,
      { cause: error },
    );
  }
  return destination;
}

/** The detached deleter's whole program; also executed verbatim by tests. */
export const TRASH_DELETER_SCRIPT = [
  `const { closeSync, openSync, readFileSync, readdirSync, rmSync, unlinkSync, writeFileSync } = require("node:fs");`,
  `const { resolve } = require("node:path");`,
  `const trash = process.argv[1];`,
  `const lock = resolve(trash, ".deleter.lock");`,
  `const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };`,
  `try {`,
  `  const existing = Number(readFileSync(lock, "utf8"));`,
  `  if (Number.isSafeInteger(existing) && existing > 0 && alive(existing)) process.exit(0);`,
  `  try { unlinkSync(lock); } catch {}`,
  `} catch {}`,
  `let descriptor;`,
  `try { descriptor = openSync(lock, "wx"); writeFileSync(descriptor, String(process.pid)); closeSync(descriptor); } catch { process.exit(0); }`,
  `let entries = [];`,
  `try { entries = readdirSync(trash); } catch {}`,
  `for (const entry of entries) {`,
  `  if (entry === ".deleter.lock") continue;`,
  `  try { rmSync(resolve(trash, entry), { recursive: true, force: true, maxRetries: 3 }); } catch {}`,
  `}`,
  `try { unlinkSync(lock); } catch {}`,
].join("\n");

/** Unlink everything in the trash without making anyone wait for it. */
export function spawnTrashDeleter(root: string): void {
  const trash = resolve(root, TRASH_DIRECTORY);
  try {
    if (readdirSync(trash).length === 0) return;
  } catch {
    return;
  }
  try {
    spawn(process.execPath, ["-e", TRASH_DELETER_SCRIPT, trash], {
      detached: true,
      stdio: "ignore",
    }).unref();
  } catch {
    // Spawning is best-effort; the next invocation sweeps the same trash.
  }
}

/** Marks `supercov/` as ours, so a project may own a directory by that name. */
const WORKSPACE_CONTAINER_MARKER = ".supercov-workspace-store";

/**
 * The instrumented workspace deliberately lives outside the dotted store.
 * Widely used libraries treat *any* dot-prefixed path segment as a hidden
 * dotfile — `send` (and therefore `express.static`, `serve-static` and
 * `res.sendFile`) answers 404 for them by default — so an application under
 * test cannot serve its own files from a dotted workspace. Copied test files
 * are kept out of the user's ordinary runner discovery by
 * `pruneCachedWorkspaceSources` at run end, not by hiding the path.
 */
export function workspaceContainerPath(root: string): string {
  return resolve(root, "supercov");
}

/** True only for a `supercov/` directory this tool created. */
function isWorkspaceContainer(path: string): boolean {
  return (
    basename(path) === "supercov" &&
    existsSync(resolve(path, WORKSPACE_CONTAINER_MARKER))
  );
}

function ensureWorkspaceContainer(root: string): string {
  const container = workspaceContainerPath(root);
  mkdirSync(container, { recursive: true });
  // Self-ignoring, so the directory never reaches the user's diff, and marked
  // so source copying can distinguish it from a project's own `supercov/`.
  for (const [file, contents] of [
    [".gitignore", "*\n"],
    [WORKSPACE_CONTAINER_MARKER, "Supercov instrumented workspace. Safe to delete.\n"],
  ] as const) {
    const path = resolve(container, file);
    if (!existsSync(path)) atomicWriteFileSync(path, contents);
  }
  return container;
}

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
  return resolve(workspaceContainerPath(root), "workspace", basename(root));
}

/**
 * Remove the copied source and test files from the stable cached workspace
 * once a run ends. Between runs those copies are pure liability: the next
 * refresh re-copies them anyway, but an ordinary test runner invoked at the
 * project root (whose default discovery does not exclude dot-directories —
 * Vitest 4 excludes only node_modules and .git) would find the copied test
 * files and silently double-count the user's suite. Keep only what the next
 * run genuinely reuses: dependency symlinks, the generated adapter directory,
 * and the instrumented build artifacts the build cache declares.
 */
export function pruneCachedWorkspaceSources(root: string): string[] {
  const workspace = cachedWorkspacePath(root);
  if (!existsSync(workspace)) return [];
  const metadata = readJson<{ artifactPaths?: string[] }>(
    resolve(workspace, ".supercov/build-cache.json"),
  );
  const keepRoots = new Set(["node_modules", ".supercov"]);
  for (const artifact of metadata?.artifactPaths ?? []) {
    const top = artifact.split(/[\\/]/)[0];
    if (top) keepRoots.add(top);
  }
  const removed: string[] = [];
  for (const entry of readdirSync(workspace, { withFileTypes: true })) {
    if (keepRoots.has(entry.name)) continue;
    removeStoredTreeDeferred(root, resolve(workspace, entry.name));
    removed.push(entry.name);
  }
  if (removed.length > 0) spawnTrashDeleter(root);
  return removed.sort();
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

  for (const path of staging) removeStoredTreeDeferred(root, path);
  for (const path of previous) removeStoredTreeDeferred(root, path);
  for (const path of invalidPrevious) removeStoredTreeDeferred(root, path);
  if (staging.length + previous.length + invalidPrevious.length > 0)
    spawnTrashDeleter(root);
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

/**
 * A published run is self-contained only once its raw-evidence archive exists.
 * At that point loose evidence and per-run state are transactional leftovers,
 * not user history. Derive every deletion target from root + run ID.
 */
export function finalizePublishedRunStorage(
  root: string,
  runId: string,
): boolean {
  const runDirectory = resolve(root, ".supercov/runs", runId);
  const publishedRun = readJson<{ id?: string }>(resolve(runDirectory, "run.json"));
  if (
    publishedRun?.id !== runId ||
    !existsSync(resolve(runDirectory, "evidence.raw.gz"))
  ) {
    return false;
  }
  removeStoredTreeDeferred(root, resolve(root, ".supercov/evidence", runId));
  removeStoredTreeDeferred(root, resolve(root, ".supercov/work", runId));
  spawnTrashDeleter(root);
  return true;
}

/** Recover dead runs and finish any publication whose atomic rename landed. */
export function recoverAbandonedRuns(root: string): string[] {
  // A prior detached deleter may have been killed with the host. Sweep its
  // durable trash before inspecting current run state.
  spawnTrashDeleter(root);
  const workRoot = resolve(root, ".supercov/work");
  if (!existsSync(workRoot)) return [];
  const recovered: string[] = [];
  for (const entry of readdirSync(workRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const path = statePath(root, entry.name);
    const state = readJson<RunState>(path);
    if (!state) continue;
    if (TERMINAL.has(state.status)) {
      finalizePublishedRunStorage(root, entry.name);
      continue;
    }
    if (processExists(state.pid)) continue;
    // Never trust a persisted path as a deletion target. A partially written
    // or manually edited state file cannot widen cleanup beyond this run's
    // deterministic workspace namespace.
    removeStoredTreeDeferred(root, isolatedWorkspacePath(root, entry.name));
    removeStoredTreeDeferred(
      root,
      resolve(root, ".supercov/work", entry.name, "run-publication"),
    );
    if (finalizePublishedRunStorage(root, entry.name)) {
      // The run directory is published by one atomic rename before the run
      // state flips terminal. Its run.json is the durable terminal record, so
      // recovery completes cleanup without recreating disposable state.
    } else {
      // Evidence moved out of the disposable workspace before run
      // generation is not a visible run. Remove that orphan on recovery so a
      // hard kill cannot accumulate partial run data indefinitely.
      removeStoredTreeDeferred(
        root,
        resolve(root, ".supercov/evidence", entry.name),
      );
      updateRunState(root, entry.name, {
        status: "abandoned",
        error: `Recovered after process ${state.pid} exited without cleanup`,
      });
    }
    recovered.push(entry.name);
  }
  if (recovered.length > 0) spawnTrashDeleter(root);
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
  hooks: CachePreparationHooks = {},
  finalDestinationRoot = destinationRoot,
): void {
  mkdirSync(destination, { recursive: true });
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    if (
      (root && ROOT_EXCLUSIONS.has(entry.name)) ||
      NESTED_SUPERCOV_EXCLUSIONS.has(entry.name)
    )
      continue;
    const from = resolve(source, entry.name);
    // Never copy our own workspace into itself. Matched by marker rather than
    // by name so a project may legitimately own a `supercov/` directory.
    if (entry.isDirectory() && isWorkspaceContainer(from)) continue;
    const to = resolve(destination, entry.name);
    const stat = lstatSync(from);
    if (stat.isDirectory())
      copyTree(
        from,
        to,
        false,
        sourceRoot,
        destinationRoot,
        hooks,
        finalDestinationRoot,
      );
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
      const targetIsDirectory = statSync(from).isDirectory();
      const relocatedAbsoluteTarget = resolve(
        destinationRoot,
        relative(sourceRoot, lexicalTarget),
      );
      // Windows junctions are absolute. Point them at the stable name that the
      // staging tree will have after publication, not the staging name that is
      // about to disappear. POSIX links remain relative and survive rename.
      const isolatedLink = process.platform === "win32" && targetIsDirectory
        ? resolve(
            finalDestinationRoot,
            relative(sourceRoot, lexicalTarget),
          )
        : isAbsolute(link)
        ? relative(
            dirname(to),
            relocatedAbsoluteTarget,
          )
        : link;
      symlinkSync(
        isolatedLink,
        to,
        process.platform === "win32"
          ? targetIsDirectory
            ? "junction"
            : "file"
          : undefined,
      );
    } else if (stat.isFile()) {
      if (hooks.copyFile) hooks.copyFile(from, to);
      else copyFileSync(from, to, constants.COPYFILE_FICLONE);
    }
    else
      throw new Error(
        `Unsupported filesystem entry in isolated project: ${relative(sourceRoot, from)}`,
      );
  }
}

/** Create a copy-on-write project snapshot whose build outputs are disposable. */
export function prepareIsolatedWorkspace(root: string, runId: string): string {
  const workspace = isolatedWorkspacePath(root, runId);
  if (removeStoredTreeDeferred(root, workspace)) spawnTrashDeleter(root);
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
  /** Internal fault-injection seam for copy fallback and ENOSPC tests. */
  copyFile?: (source: string, destination: string) => void;
  /** Exact-fingerprint build artifacts carried into the refreshed snapshot. */
  reusePaths?: string[];
  /** Internal seam for simulating platform rename failures. */
  rename?: (source: string, destination: string) => void;
}

/** Refresh the stable, disposable instrumented-build namespace. */
export function prepareCachedWorkspace(
  root: string,
  hooks: CachePreparationHooks = {},
): string {
  const workspace = cachedWorkspacePath(root);
  ensureWorkspaceContainer(root);
  recoverCachedWorkspace(root);
  const staging = cacheTransactionPath(root, "staging");
  const previous = cacheTransactionPath(root, "previous");
  let movedPrevious = false;
  try {
    copyTree(root, staging, true, root, staging, hooks, workspace);
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
    for (const requested of hooks.reusePaths ?? []) {
      const from = resolve(workspace, requested);
      const to = resolve(staging, requested);
      if (
        !inside(workspace, from) ||
        from === workspace ||
        !inside(staging, to) ||
        to === staging ||
        !existsSync(from)
      ) {
        throw new Error(`Refusing to reuse unexpected build path: ${requested}`);
      }
      const stat = lstatSync(from);
      if (stat.isDirectory())
        copyTree(from, to, false, workspace, staging, hooks, workspace);
      else if (stat.isFile()) {
        mkdirSync(dirname(to), { recursive: true });
        if (hooks.copyFile) hooks.copyFile(from, to);
        else copyFileSync(from, to, constants.COPYFILE_FICLONE);
      } else {
        throw new Error(`Unsupported reusable build entry: ${requested}`);
      }
    }
    hooks.beforePublish?.(staging);

    // Keep the last complete cache available for the whole refresh. These two
    // same-filesystem renames are the only publication boundary. Recovery
    // restores `previous` if the process is killed in the narrow gap.
    const publishRename = hooks.rename ?? atomicRenameSync;
    if (existsSync(workspace)) {
      publishRename(workspace, previous);
      movedPrevious = true;
      hooks.afterPreviousMoved?.(previous);
    }
    publishRename(staging, workspace);
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
    if (removeStoredTreeDeferred(root, staging)) spawnTrashDeleter(root);
  }

  // Failure to remove an obsolete previous generation must not invalidate the
  // newly published cache. The next invocation and `supercov clean` both
  // remove it deterministically.
  try {
    if (removeStoredTreeDeferred(root, previous)) spawnTrashDeleter(root);
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
  if (removeStoredTreeDeferred(root, workspace)) spawnTrashDeleter(root);
}

export interface CleanupOptions {
  keep: number;
  dryRun: boolean;
}

export interface CleanupResult {
  removedRuns: string[];
  removedWorkspaces: string[];
  removedEvidence: string[];
  removedBuildCache: boolean;
}

/** Deterministic retention: IDs sort newest-first because run IDs are UTC. */
function cleanCoverageStorageLocked(
  root: string,
  options: CleanupOptions,
  removeBuildCache: boolean,
): CleanupResult {
  recoverAbandonedRuns(root);
  const runsRoot = resolve(root, ".supercov/runs");
  const workRoot = resolve(root, ".supercov/work");
  const evidenceRoot = resolve(root, ".supercov/evidence");
  const bases = [runsRoot, workRoot, evidenceRoot];
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
  const published = existsSync(runsRoot)
    ? readdirSync(runsRoot, { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .map((entry) => entry.name)
        .sort((left, right) => right.localeCompare(left))
    : [];
  const retained = new Set(
    published.filter((id) => !active.has(id)).slice(0, Math.max(0, options.keep)),
  );
  const removedRuns: string[] = [];
  const removedWorkspaces: string[] = [];
  const removedEvidence: string[] = [];
  for (const id of ordered) {
    const work = resolve(root, ".supercov/work", id);
    const state = readJson<RunState>(resolve(work, "state.json"));
    if (active.has(id)) continue;
    const hasPublishedRun = existsSync(resolve(runsRoot, id));
    const removeHistory = hasPublishedRun && !retained.has(id);
    const removeTransientWork =
      existsSync(work) && (!state || TERMINAL.has(state.status));
    if (removeTransientWork) {
      removedWorkspaces.push(id);
      if (!options.dryRun) removeStoredTreeDeferred(root, work);
    }
    const hasLooseEvidence = existsSync(resolve(evidenceRoot, id));
    if (hasLooseEvidence && (!hasPublishedRun || removeHistory)) {
      removedEvidence.push(id);
      if (!options.dryRun)
        removeStoredTreeDeferred(root, resolve(evidenceRoot, id));
    }
    if (removeHistory) {
      removedRuns.push(id);
      if (!options.dryRun)
        removeStoredTreeDeferred(root, resolve(runsRoot, id));
    }
  }
  const buildCache = workspaceContainerPath(root);
  // Releases before the workspace was moved out of the dotted store used
  // both of these names. `clean` is the explicit destructive command, so it
  // must migrate them too; otherwise a project can retain millions of files
  // in a layout the current engine will never reuse. `prune` deliberately
  // preserves every cache generation.
  const legacyBuildCaches = [
    resolve(root, ".supercov/.cache"),
    resolve(root, ".supercov/cache"),
  ];
  const removableBuildCaches =
    removeBuildCache && active.size === 0
      ? [
          ...(existsSync(buildCache) && isWorkspaceContainer(buildCache)
            ? [buildCache]
            : []),
          ...legacyBuildCaches.filter((path) => existsSync(path)),
        ]
      : [];
  const removedBuildCache = removableBuildCaches.length > 0;
  if (!options.dryRun)
    for (const path of removableBuildCaches)
      removeStoredTreeDeferred(root, path);
  if (
    !options.dryRun &&
    (removedRuns.length > 0 ||
      removedWorkspaces.length > 0 ||
      removedEvidence.length > 0 ||
      removedBuildCache)
  )
    spawnTrashDeleter(root);
  return { removedRuns, removedWorkspaces, removedEvidence, removedBuildCache };
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
    return cleanCoverageStorageLocked(root, options, true);
  } finally {
    lock.release();
  }
}

/** Explicit history retention that deliberately preserves the shared cache. */
export function pruneCoverageStorage(
  root: string,
  options: CleanupOptions,
): CleanupResult {
  const lock = acquireProjectLock(
    root,
    `prune-${process.pid}-${randomUUID()}`,
  );
  try {
    return cleanCoverageStorageLocked(root, options, false);
  } finally {
    lock.release();
  }
}
