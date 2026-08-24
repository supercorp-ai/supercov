import { basename } from "node:path";
import { spawnSync, type ChildProcess } from "node:child_process";

export const PROCESS_SUPERVISION_SCHEMA_VERSION = 1;
export const DEFAULT_DIAGNOSTIC_INTERVAL_MS = 60_000;
export const COMMAND_TERMINATION_GRACE_MS = 5_000;
export const COMMAND_TIMEOUT_EXIT_CODE = 124;

export interface ProcessSnapshot {
  pid: number;
  parentPid: number;
  state?: string;
  cpuSeconds?: number;
  executable: string;
}

export interface ProcessWatchdogOptions {
  diagnosticIntervalMs: number;
  timeoutMs?: number;
  write: (message: string) => void;
  onTimeout: () => void;
}

export interface ProcessWatchdog {
  stop: () => void;
}

function cpuSeconds(value: string): number | undefined {
  const dayParts = value.split("-");
  const clock = dayParts.at(-1)?.split(":").map(Number) ?? [];
  if (clock.some((part) => !Number.isFinite(part))) return undefined;
  const days = dayParts.length === 2 ? Number(dayParts[0]) : 0;
  if (!Number.isFinite(days)) return undefined;
  const [hours = 0, minutes = 0, seconds = 0] =
    clock.length === 3 ? clock : [0, ...clock];
  return days * 86_400 + hours * 3_600 + minutes * 60 + seconds;
}

function posixProcesses(): ProcessSnapshot[] {
  const result = spawnSync(
    "ps",
    ["-axo", "pid=,ppid=,state=,time=,comm="],
    { encoding: "utf8", timeout: 5_000 },
  );
  if (result.status !== 0) return [];
  return result.stdout
    .split("\n")
    .flatMap((line) => {
      const match = /^\s*(\d+)\s+(\d+)\s+(\S+)\s+(\S+)\s+(.+?)\s*$/.exec(line);
      if (!match) return [];
      return [{
        pid: Number(match[1]),
        parentPid: Number(match[2]),
        state: match[3],
        cpuSeconds: cpuSeconds(match[4]!),
        executable: basename(match[5]!),
      }];
    });
}

function windowsProcesses(): ProcessSnapshot[] {
  const script = [
    "Get-CimInstance Win32_Process",
    "Select-Object ProcessId,ParentProcessId,Name,KernelModeTime,UserModeTime",
    "ConvertTo-Json -Compress",
  ].join(" | ");
  const result = spawnSync(
    "powershell.exe",
    ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
    { encoding: "utf8", timeout: 10_000 },
  );
  if (result.status !== 0 || !result.stdout.trim()) return [];
  try {
    const decoded = JSON.parse(result.stdout) as
      | Record<string, unknown>
      | Array<Record<string, unknown>>;
    return (Array.isArray(decoded) ? decoded : [decoded]).flatMap((entry) => {
      const pid = Number(entry["ProcessId"]);
      const parentPid = Number(entry["ParentProcessId"]);
      if (!Number.isSafeInteger(pid) || !Number.isSafeInteger(parentPid))
        return [];
      const kernel = Number(entry["KernelModeTime"] ?? 0);
      const user = Number(entry["UserModeTime"] ?? 0);
      return [{
        pid,
        parentPid,
        cpuSeconds:
          Number.isFinite(kernel + user) ? (kernel + user) / 10_000_000 : undefined,
        executable: basename(String(entry["Name"] ?? "unknown")),
      }];
    });
  } catch {
    return [];
  }
}

export function descendantProcessTree(rootPid: number): ProcessSnapshot[] {
  const processes = process.platform === "win32"
    ? windowsProcesses()
    : posixProcesses();
  const descendants = new Set([rootPid]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const entry of processes) {
      if (descendants.has(entry.parentPid) && !descendants.has(entry.pid)) {
        descendants.add(entry.pid);
        changed = true;
      }
    }
  }
  return processes
    .filter((entry) => descendants.has(entry.pid))
    .sort((left, right) => left.pid - right.pid);
}

function duration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${Math.round(milliseconds)}ms`;
  const seconds = Math.round(milliseconds / 1_000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m${String(seconds % 60).padStart(2, "0")}s`;
}

export function formatProcessDiagnostic(
  rootPid: number,
  elapsedMs: number,
  tree = descendantProcessTree(rootPid),
): string {
  const entries = tree.length > 0
    ? tree.map((entry) =>
        [
          `pid=${entry.pid}`,
          `ppid=${entry.parentPid}`,
          `exe=${entry.executable}`,
          ...(entry.state ? [`state=${entry.state}`] : []),
          ...(entry.cpuSeconds === undefined
            ? []
            : [`cpu=${entry.cpuSeconds.toFixed(1)}s`]),
        ].join(" "),
      )
    : [`pid=${rootPid} process details unavailable`];
  return `[supercov] command still running after ${duration(elapsedMs)}\n${entries
    .map((entry) => `  ${entry}`)
    .join("\n")}`;
}

/**
 * Prevent an arbitrary wrapped command from ever hanging silently. Diagnostics
 * are observational; termination occurs only when the user supplied a timeout.
 * Never signal descendants to request diagnostics: arbitrary launch trees can
 * contain Node processes without Supercov's signal handler, and SIGUSR2 would
 * terminate those otherwise healthy processes. The preloaded Node runtime
 * reports its own active resources through a signal-free elected timer.
 */
export function startProcessWatchdog(
  child: ChildProcess,
  options: ProcessWatchdogOptions,
): ProcessWatchdog {
  if (!child.pid) return { stop() {} };
  const startedAt = Date.now();
  const report = (timedOut: boolean): void => {
    const tree = descendantProcessTree(child.pid!);
    options.write(formatProcessDiagnostic(child.pid!, Date.now() - startedAt, tree));
    if (timedOut) options.onTimeout();
  };
  const diagnostics = setInterval(
    () => report(false),
    options.diagnosticIntervalMs,
  );
  diagnostics.unref();
  const timeout = options.timeoutMs === undefined
    ? undefined
    : setTimeout(() => report(true), options.timeoutMs);
  timeout?.unref();
  let stopped = false;
  return {
    stop() {
      if (stopped) return;
      stopped = true;
      clearInterval(diagnostics);
      if (timeout) clearTimeout(timeout);
    },
  };
}

export function positiveMilliseconds(
  value: string | undefined,
  name: string,
): number | undefined {
  if (value === undefined || value === "") return undefined;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1)
    throw new Error(`${name} must be a positive integer number of milliseconds`);
  return parsed;
}
