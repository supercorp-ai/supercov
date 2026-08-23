import type { CoverageCarrier } from "./types.ts";
import {
  bindCoverageContext,
  coverageCarrier,
  withCoverageCarrier,
} from "./runtime.ts";
import {
  decodeCoverageCarrier,
  encodeCoverageCarrier,
} from "./transport.ts";

export const COVERAGE_JOB_FIELD = "__supercov";

type RecordValue = Record<string, unknown>;

function record(value: unknown): RecordValue | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as RecordValue)
    : undefined;
}

export function injectCoverageCarrier<T>(
  payload: T,
  carrier = coverageCarrier(),
): T {
  const object = record(payload);
  if (!object) return payload;
  return {
    ...object,
    [COVERAGE_JOB_FIELD]: encodeCoverageCarrier(carrier),
  } as T;
}

export function extractCoverageCarrier(
  payload: unknown,
): CoverageCarrier | undefined {
  const encoded = record(payload)?.[COVERAGE_JOB_FIELD];
  return decodeCoverageCarrier(
    typeof encoded === "string" ? encoded : undefined,
  );
}

export function wrapQueuePublisher<
  T extends (...args: never[]) => unknown,
>(publisher: T, payloadIndex = 0): T {
  return function coverageQueuePublisher(
    this: unknown,
    ...args: Parameters<T>
  ): ReturnType<T> {
    const scoped = [...args] as unknown[];
    scoped[payloadIndex] = injectCoverageCarrier(scoped[payloadIndex]);
    return Reflect.apply(publisher, this, scoped) as ReturnType<T>;
  } as T;
}

export function wrapQueueProcessor<
  T extends (...args: never[]) => unknown,
>(processor: T, payload: (args: Parameters<T>) => unknown): T {
  return function coverageQueueProcessor(
    this: unknown,
    ...args: Parameters<T>
  ): ReturnType<T> {
    const carrier = extractCoverageCarrier(payload(args));
    return withCoverageCarrier(carrier, () =>
      Reflect.apply(processor, this, args),
    ) as ReturnType<T>;
  } as T;
}

/** BullMQ and Bee-Queue jobs expose user data through `job.data`. */
export function wrapBullProcessor<T extends (...args: never[]) => unknown>(
  processor: T,
): T {
  return wrapQueueProcessor(processor, (args) =>
    record(args[0])?.["data"],
  );
}

/** pg-boss handlers receive either one job or a batch whose jobs have `data`. */
export function wrapPgBossProcessor<T extends (...args: never[]) => unknown>(
  processor: T,
): T {
  return wrapQueueProcessor(processor, (args) => {
    const first = args[0] as unknown;
    const job = Array.isArray(first) ? first[0] : first;
    return record(job)?.["data"];
  });
}

/** Agenda stores user data in `job.attrs.data`. */
export function wrapAgendaProcessor<T extends (...args: never[]) => unknown>(
  processor: T,
): T {
  return wrapQueueProcessor(processor, (args) =>
    record(record(args[0])?.["attrs"])?.["data"],
  );
}

/** Capture context for in-process scheduler callbacks without changing payloads. */
export function wrapScheduledCallback<
  T extends (...args: never[]) => unknown,
>(callback: T): T {
  return bindCoverageContext(callback);
}
