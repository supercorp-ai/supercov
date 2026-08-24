import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { isolateCollectorRuntime } from "../../src/runtimeIsolation.ts";

describe("collector runtime isolation", () => {
  it("replaces only the runtime key assignment and leaves the application sentinel", () => {
    const source = `
      var runtimeInstanceToken = "__SUPERCOV_RUNTIME_INSTANCE__";
      var runtimeInstance = runtimeInstanceToken === "__SUPERCOV_RUNTIME_INSTANCE__"
        ? "application"
        : runtimeInstanceToken;
    `;
    const isolated = isolateCollectorRuntime(source, "collector-run");
    expect(isolated).toContain('runtimeInstanceToken = "collector-run"');
    expect(isolated).toContain(
      'runtimeInstanceToken === "__SUPERCOV_RUNTIME_INSTANCE__"',
    );
  });
});
