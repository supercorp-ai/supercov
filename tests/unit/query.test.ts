import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  locationSelector,
  resolveCoverageQueryInvocation,
} from "../../src/query.ts";

describe("coverage query routing", () => {
  it("treats the optional trailing location number as a column", () => {
    expect(locationSelector("src/permission.js:12:7")).toEqual({
      file: "src/permission.js",
      line: 12,
    });
    expect(locationSelector("C:\\repo\\source.ts:12:7")).toEqual({
      file: "C:\\repo\\source.ts",
      line: 12,
    });
  });

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

  it("rejects a run query that omits the coverage segment", () => {
    expect(() =>
      resolveCoverageQueryInvocation("runs", ["run-123", "file", "src/x.ts"]),
    ).toThrow(/Unknown runs query: file/);
  });

  it("rejects a run ID without any coverage query", () => {
    expect(() =>
      resolveCoverageQueryInvocation("runs", ["run-123"]),
    ).toThrow(/Missing coverage query after run run-123/);
    expect(() =>
      resolveCoverageQueryInvocation("runs", ["run-123", "--json"]),
    ).toThrow(/Missing coverage query after run run-123/);
  });

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
