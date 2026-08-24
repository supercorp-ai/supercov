import { spawn } from "node:child_process";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  formatProcessDiagnostic,
  positiveMilliseconds,
  startProcessWatchdog,
} from "../../src/processDiagnostics.ts";

describe("long-running command diagnostics", () => {
  it("formats a sanitized process tree without command arguments", () => {
    const output = formatProcessDiagnostic(20, 61_000, [
      {
        pid: 20,
        parentPid: 10,
        executable: "node",
        state: "S",
        cpuSeconds: 1.25,
      },
    ]);
    expect(output).toContain("still running after 1m01s");
    expect(output).toContain("pid=20 ppid=10 exe=node state=S cpu=1.3s");
    expect(output).not.toContain("argv");
  });

  it("reports a quiet process and applies only an explicitly configured timeout", async () => {
    const child = spawn(
      process.execPath,
      [
        "-e",
        "setInterval(() => {}, 1000);",
      ],
      { stdio: "ignore" },
    );
    const messages: string[] = [];
    let timeouts = 0;
    const watchdog = startProcessWatchdog(child, {
      diagnosticIntervalMs: 25,
      timeoutMs: 90,
      write(message) {
        messages.push(message);
      },
      onTimeout() {
        timeouts += 1;
        child.kill("SIGTERM");
      },
    });
    await new Promise<void>((resolve, reject) => {
      const safety = setTimeout(() => {
        child.kill("SIGKILL");
        reject(new Error("watchdog fixture did not terminate"));
      }, 2_000);
      child.once("close", () => {
        clearTimeout(safety);
        resolve();
      });
    });
    watchdog.stop();
    expect(timeouts).toBe(1);
    expect(messages.length).toBeGreaterThan(1);
    expect(messages.some((message) => message.includes(`pid=${child.pid}`))).toBe(
      true,
    );
  });

  it("never signals a healthy arbitrary Node descendant for diagnostics", async () => {
    const child = spawn(
      process.execPath,
      ["-e", "setTimeout(() => process.exit(0), 100)"],
      { stdio: "ignore" },
    );
    const watchdog = startProcessWatchdog(child, {
      diagnosticIntervalMs: 20,
      write() {},
      onTimeout() {
        throw new Error("an observational watchdog must not time out");
      },
    });
    const result = await new Promise<{ code: number | null; signal: NodeJS.Signals | null }>(
      (resolve) => child.once("close", (code, signal) => resolve({ code, signal })),
    );
    watchdog.stop();
    expect(result).toEqual({ code: 0, signal: null });
  });

  it("rejects invalid diagnostic and timeout environment values", () => {
    expect(positiveMilliseconds(undefined, "VALUE")).toBeUndefined();
    expect(positiveMilliseconds("50", "VALUE")).toBe(50);
    expect(() => positiveMilliseconds("0", "VALUE")).toThrow(
      /positive integer/,
    );
    expect(() => positiveMilliseconds("1.5", "VALUE")).toThrow(
      /positive integer/,
    );
  });
});
