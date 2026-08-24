import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { SupercovError } from "./agentJson.ts";
import type { McdcDecisionResult } from "./types.ts";

export const WAIVERS_FILE = "supercov.waivers.json";
export const WAIVERS_SCHEMA_VERSION = 1;

/**
 * A reviewed statement that one MC/DC condition has no satisfiable
 * independence pair. Waivers never change measured coverage: waived
 * conditions stay uncovered in every raw total and are reported as a
 * separate category alongside them.
 */
export interface CoverageWaiver {
  file: string;
  /** Decision ID or the decision's exact source text (whitespace-insensitive). */
  decision?: string;
  /** Disambiguates identical decision sources at different locations. */
  line?: number;
  /** Condition source text (whitespace-insensitive), or "C<n>" with a decision. */
  condition: string;
  reason: string;
}

export interface CoverageWaiverMatch {
  waiver: CoverageWaiver;
  decisionId: string;
  file: string;
  line: number;
  conditionIndex: number;
  conditionSource: string;
  /** A waiver on a condition that is actually covered contradicts the review. */
  covered: boolean;
}

export interface CoverageWaiverEvaluation {
  path: string;
  waivers: CoverageWaiver[];
  /** Uncovered conditions excused by a reviewed waiver. */
  applied: CoverageWaiverMatch[];
  /** Waivers whose condition the run proves covered. */
  contradicted: CoverageWaiverMatch[];
  /** Waivers that match no decision condition in this run. */
  unmatched: CoverageWaiver[];
  /** decision ID -> condition index -> waiver, for annotating views. */
  waivedByDecision: Map<string, Map<number, CoverageWaiver>>;
  /** file -> count of applied waivers, for per-file gap views. */
  appliedByFile: Map<string, number>;
}

function normalizedSource(source: string): string {
  return source.replace(/\s+/g, " ").trim();
}

function invalidWaiver(index: number, problem: string): SupercovError {
  return new SupercovError(
    "INVALID_ARGUMENT",
    `${WAIVERS_FILE} waiver ${index + 1} ${problem}`,
    { details: { file: WAIVERS_FILE, waiver: index + 1 } },
  );
}

export function readCoverageWaivers(
  root: string,
): { path: string; waivers: CoverageWaiver[] } | undefined {
  const path = resolve(root, WAIVERS_FILE);
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new SupercovError(
      "INVALID_ARGUMENT",
      `${WAIVERS_FILE} is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
      { details: { file: WAIVERS_FILE } },
    );
  }
  const record = parsed as { version?: unknown; waivers?: unknown };
  if (record?.version !== WAIVERS_SCHEMA_VERSION || !Array.isArray(record.waivers)) {
    throw new SupercovError(
      "INVALID_ARGUMENT",
      `${WAIVERS_FILE} must be {"version": 1, "waivers": [...]}`,
      { details: { file: WAIVERS_FILE } },
    );
  }
  const waivers = record.waivers.map((entry, index) => {
    const waiver = entry as Partial<CoverageWaiver>;
    if (typeof waiver?.file !== "string" || waiver.file.length === 0)
      throw invalidWaiver(index, "requires a non-empty file");
    if (typeof waiver.condition !== "string" || waiver.condition.length === 0)
      throw invalidWaiver(index, "requires a non-empty condition");
    if (typeof waiver.reason !== "string" || waiver.reason.trim().length === 0)
      throw invalidWaiver(index, "requires a non-empty reason");
    if (waiver.decision !== undefined && typeof waiver.decision !== "string")
      throw invalidWaiver(index, "has a non-string decision");
    if (
      waiver.line !== undefined &&
      (!Number.isSafeInteger(waiver.line) || waiver.line < 1)
    )
      throw invalidWaiver(index, "has a non-positive line");
    if (/^C\d+$/.test(waiver.condition) && !waiver.decision)
      throw invalidWaiver(
        index,
        `uses the positional condition ${waiver.condition} without a decision`,
      );
    return {
      file: waiver.file,
      ...(waiver.decision !== undefined ? { decision: waiver.decision } : {}),
      ...(waiver.line !== undefined ? { line: waiver.line } : {}),
      condition: waiver.condition,
      reason: waiver.reason,
    };
  });
  return { path, waivers };
}

export function evaluateCoverageWaivers(
  decisions: McdcDecisionResult[],
  source: { path: string; waivers: CoverageWaiver[] },
): CoverageWaiverEvaluation {
  const applied: CoverageWaiverMatch[] = [];
  const contradicted: CoverageWaiverMatch[] = [];
  const unmatched: CoverageWaiver[] = [];
  const waivedByDecision = new Map<string, Map<number, CoverageWaiver>>();
  const appliedByFile = new Map<string, number>();

  for (const waiver of source.waivers) {
    const matches: CoverageWaiverMatch[] = [];
    for (const decision of decisions) {
      if (decision.meta.file !== waiver.file) continue;
      if (waiver.line !== undefined && decision.meta.line !== waiver.line)
        continue;
      if (
        waiver.decision !== undefined &&
        decision.meta.id !== waiver.decision &&
        normalizedSource(decision.meta.source) !==
          normalizedSource(waiver.decision)
      )
        continue;
      for (const condition of decision.conditions) {
        const positional = `C${condition.index + 1}`;
        if (
          waiver.condition !== positional &&
          normalizedSource(condition.source) !==
            normalizedSource(waiver.condition)
        )
          continue;
        matches.push({
          waiver,
          decisionId: decision.meta.id,
          file: decision.meta.file,
          line: decision.meta.line,
          conditionIndex: condition.index,
          conditionSource: condition.source,
          covered: condition.covered,
        });
      }
    }
    if (matches.length === 0) {
      unmatched.push(waiver);
      continue;
    }
    for (const match of matches) {
      if (match.covered) {
        contradicted.push(match);
        continue;
      }
      const byCondition =
        waivedByDecision.get(match.decisionId) ??
        new Map<number, CoverageWaiver>();
      if (byCondition.has(match.conditionIndex)) continue;
      applied.push(match);
      byCondition.set(match.conditionIndex, match.waiver);
      waivedByDecision.set(match.decisionId, byCondition);
      appliedByFile.set(match.file, (appliedByFile.get(match.file) ?? 0) + 1);
    }
  }
  return {
    path: source.path,
    waivers: source.waivers,
    applied,
    contradicted,
    unmatched,
    waivedByDecision,
    appliedByFile,
  };
}
