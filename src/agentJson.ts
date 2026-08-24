import { Buffer } from "node:buffer";

export const AGENT_JSON_SCHEMA_VERSION = 1 as const;
export const AGENT_JSON_MAX_BYTES = 64 * 1024;

export const SUPERCOV_ERROR_CODES = [
  "AMBIGUOUS_SELECTOR",
  "DECISION_NOT_FOUND",
  "FILTER_UNAVAILABLE",
  "INTERNAL_ERROR",
  "INVALID_ARGUMENT",
  "MINIMIZATION_COMPLEXITY_LIMIT",
  "NO_RUNS",
  "RESPONSE_TOO_LARGE",
  "RUN_NOT_FOUND",
  "SCOPE_UNAVAILABLE",
  "SOURCE_NOT_FOUND",
  "TARGET_UNREACHABLE",
  "TEST_FILTER_EMPTY",
  "TEST_NOT_FOUND",
  "UNATTRIBUTED_EVIDENCE",
  "UNKNOWN_COMMAND",
] as const;

export type SupercovErrorCode = (typeof SUPERCOV_ERROR_CODES)[number];

export interface AgentJsonPagination {
  offset: number;
  limit: number;
  returned: number;
  total: number;
  hasMore: boolean;
  nextOffset: number | null;
}

export interface AgentJsonSuccess {
  schemaVersion: typeof AGENT_JSON_SCHEMA_VERSION;
  ok: true;
  command: string;
  data: unknown;
  pagination?: AgentJsonPagination;
}

export interface AgentJsonFailure {
  schemaVersion: typeof AGENT_JSON_SCHEMA_VERSION;
  ok: false;
  command?: string;
  error: {
    code: SupercovErrorCode;
    message: string;
    retryable: boolean;
    details?: Record<string, unknown>;
  };
}

export class SupercovError extends Error {
  readonly code: SupercovErrorCode;
  readonly retryable: boolean;
  readonly details?: Record<string, unknown>;

  constructor(
    code: SupercovErrorCode,
    message: string,
    options: {
      retryable?: boolean;
      details?: Record<string, unknown>;
      cause?: unknown;
    } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "SupercovError";
    this.code = code;
    this.retryable = options.retryable ?? false;
    this.details = options.details;
  }
}

export function agentPagination(
  offset: number,
  limit: number,
  returned: number,
  total: number,
): AgentJsonPagination {
  const nextOffset = offset + returned;
  const hasMore = returned > 0 && nextOffset < total;
  return {
    offset,
    limit,
    returned,
    total,
    hasMore,
    nextOffset: hasMore ? nextOffset : null,
  };
}

function serialized(value: unknown): string {
  return `${JSON.stringify(value)}\n`;
}

export function agentSuccessJson(
  command: string,
  data: unknown,
  pagination?: AgentJsonPagination,
): string {
  const envelope: AgentJsonSuccess = {
    schemaVersion: AGENT_JSON_SCHEMA_VERSION,
    ok: true,
    command,
    data,
    ...(pagination ? { pagination } : {}),
  };
  const json = serialized(envelope);
  const bytes = Buffer.byteLength(json);
  if (bytes > AGENT_JSON_MAX_BYTES) {
    throw new SupercovError(
      "RESPONSE_TOO_LARGE",
      `JSON response is ${bytes} bytes; the maximum is ${AGENT_JSON_MAX_BYTES} bytes`,
      {
        details: {
          actualBytes: bytes,
          maxBytes: AGENT_JSON_MAX_BYTES,
          hint: "Use --offset/--limit or a narrower coverage query.",
        },
      },
    );
  }
  return json;
}

export function asSupercovError(error: unknown): SupercovError {
  if (error instanceof SupercovError) return error;
  return new SupercovError(
    "INTERNAL_ERROR",
    error instanceof Error ? error.message : String(error),
    { cause: error },
  );
}

export function agentFailureJson(
  error: unknown,
  command?: string,
): string {
  const failure = asSupercovError(error);
  const envelope: AgentJsonFailure = {
    schemaVersion: AGENT_JSON_SCHEMA_VERSION,
    ok: false,
    ...(command ? { command } : {}),
    error: {
      code: failure.code,
      message: failure.message,
      retryable: failure.retryable,
      ...(failure.details ? { details: failure.details } : {}),
    },
  };
  const json = serialized(envelope);
  if (Buffer.byteLength(json) <= AGENT_JSON_MAX_BYTES) return json;
  return serialized({
    schemaVersion: AGENT_JSON_SCHEMA_VERSION,
    ok: false,
    ...(command ? { command } : {}),
    error: {
      code: failure.code,
      message: failure.message.slice(0, 1_000),
      retryable: failure.retryable,
    },
  } satisfies AgentJsonFailure);
}
