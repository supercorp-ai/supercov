import { describe, expect, it } from "vitest";
import { resolveCoverageQueryInvocation } from "../../src/query";

describe("coverage query routing", () => {
  it("keeps runs without an ID as the run listing", () => {
    expect(resolveCoverageQueryInvocation("runs", ["--limit", "5"])).toEqual({
      command: "runs",
      args: ["--limit", "5"],
    });
  });

  it("resolves a run's coverage resource to its summary", () => {
    expect(
      resolveCoverageQueryInvocation("runs", ["run-123", "coverage", "--json"]),
    ).toEqual({
      command: "summary",
      args: ["--run", "run-123", "--json"],
    });
  });

  it.each(["files", "gaps", "file", "decision", "covers", "test"])(
    "resolves the coverage %s subresource",
    (child) => {
      expect(
        resolveCoverageQueryInvocation("runs", [
          "run-123",
          "coverage",
          child,
          "target",
          "--json",
        ]),
      ).toEqual({
        command: child,
        args: ["--run", "run-123", "target", "--json"],
      });
    },
  );

  it("rejects unknown coverage children", () => {
    expect(() =>
      resolveCoverageQueryInvocation("runs", [
        "run-123",
        "coverage",
        "unknown",
      ]),
    ).toThrow("Unknown coverage query: unknown");
  });
});
