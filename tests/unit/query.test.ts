import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { resolveCoverageQueryInvocation } from "../../src/query.ts";

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

  for (const child of ["scope", "files", "gaps", "file", "decision", "covers", "test", "minimize"]) {
    it(`resolves the coverage ${child} subresource`, () => {
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
    });
  }

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
