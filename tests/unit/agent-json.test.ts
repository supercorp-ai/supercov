import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  AGENT_JSON_MAX_BYTES,
  agentFailureJson,
  agentPagination,
  agentSuccessJson,
  SupercovError,
} from "../../src/agentJson.ts";

function golden(name: string): string {
  return readFileSync(resolve("tests/golden", name), "utf8");
}

describe("agent JSON contract v1", () => {
  it("keeps the success envelope byte-for-byte stable", () => {
    expect(
      agentSuccessJson("coverage.summary", {
        run: "run-123",
        coverage: { lines: 100 },
      }),
    ).toBe(golden("agent-success.json"));
  });

  it("uses one stable pagination shape", () => {
    expect(
      agentSuccessJson(
        "coverage.gaps",
        { gaps: [{ file: "src/example.ts" }] },
        agentPagination(20, 20, 1, 21),
      ),
    ).toBe(golden("agent-page.json"));
  });

  it("keeps structured failures byte-for-byte stable", () => {
    expect(
      agentFailureJson(
        new SupercovError(
          "SOURCE_NOT_FOUND",
          "Source file not found: missing.ts",
          { details: { selector: "missing.ts" } },
        ),
        "coverage.file",
      ),
    ).toBe(golden("agent-error.json"));
  });

  it("rejects responses beyond the agent context budget", () => {
    try {
      agentSuccessJson("coverage.file", { source: "x".repeat(AGENT_JSON_MAX_BYTES) });
      throw new Error("expected response budget failure");
    } catch (error) {
      expect(error).toMatchObject({
        code: "RESPONSE_TOO_LARGE",
        retryable: false,
        details: {
          maxBytes: AGENT_JSON_MAX_BYTES,
          hint: "Use --offset/--limit or a narrower coverage query.",
        },
      });
    }
  });
});
