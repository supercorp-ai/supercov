import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  COVERAGE_JOB_FIELD,
  injectCoverageCarrier,
  wrapBullProcessor,
  wrapQueuePublisher,
} from "../../src/queueAdapters.ts";
import { coverageCarrier, withCoverageCarrier } from "../../src/runtime.ts";
import type { CoverageExecutionScope } from "../../src/types.ts";

const scope: CoverageExecutionScope = {
  version: 1,
  runId: "run",
  workerId: "worker",
  testId: "test",
  testKey: "test-key",
  retry: 0,
  attemptId: "attempt",
};

describe("queue context adapters", () => {
  it("injects a carrier at publish and restores it in a BullMQ-style processor", async () => {
    const published: unknown[] = [];
    const publish = wrapQueuePublisher((payload: unknown) => {
      published.push(payload);
      return payload;
    });
    const payload = withCoverageCarrier(
      { version: 1, scope, phaseId: "assertion" },
      () => publish({ articleId: 1 }),
    ) as Record<string, unknown>;
    expect(payload[COVERAGE_JOB_FIELD]).toEqual(expect.any(String));

    const processor = wrapBullProcessor(async (job: { data: unknown }) => ({
      job,
      carrier: coverageCarrier(),
    }));
    await expect(processor({ data: payload })).resolves.toMatchObject({
      carrier: { scope, phaseId: "assertion" },
    });
  });

  it("leaves scalar payloads unchanged", () => {
    expect(injectCoverageCarrier("job")).toBe("job");
  });
});
