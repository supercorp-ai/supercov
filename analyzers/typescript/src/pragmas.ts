import type ts from "typescript";
import type { Site } from "./types.js";

export type AssertionPhase = { source?: string; op: string; status?: string };
export function assertionWitnessIssue(
  phases: AssertionPhase[] | undefined,
  source?: string,
  method?: string,
) {
  const matches = (phases ?? []).filter(
    (p) => p.source === source && p.op.split(".").pop() === method,
  );
  if (source && matches.length && matches.every((p) => p.status === "passed"))
    return undefined;
  const statuses = new Set(matches.map((p) => p.status ?? "unknown"));
  return !phases
    ? ("capture-unavailable" as const)
    : !source
      ? ("uninstrumented-observation" as const)
      : !matches.length
        ? ("call-not-recorded" as const)
        : statuses.size > 1
          ? ("mixed-call-outcomes" as const)
          : statuses.has("failed")
            ? ("call-failed" as const)
            : ("call-incomplete" as const);
}

interface Attachment {
  testKey: string;
  source: string;
  method: string;
  inert: boolean;
}
interface Comment {
  id: string;
  where: string;
  raw: string;
  target?: { file: string; function: string; snippet?: string; via?: string };
  candidateSites: string[];
  issue?: string;
  attachments: Attachment[];
}
export interface PragmaHint {
  id: string;
  where: string;
  raw: string;
  target?: Comment["target"];
  candidateSites: string[];
  issue?: string;
  test?: string;
  assertionSource?: string;
  assertionMethod?: string;
  witness: "passed" | "unavailable";
  witnessIssue?: string;
}

/** Source hints are kept OUT of observations. A comment never adds a boundary. */
export function collectPragmas(
  compiler: typeof ts,
  files: ts.SourceFile[],
  relativeFile: (file: ts.SourceFile) => string,
  sites: Site[],
) {
  const comments = new Map<string, Comment>();
  const key = (sf: ts.SourceFile, pos: number) => `${relativeFile(sf)}:${pos}`;
  const location = (sf: ts.SourceFile, pos: number) => {
    const p = sf.getLineAndCharacterOfPosition(pos);
    return `${relativeFile(sf)}:${p.line + 1}:${p.character + 1}`;
  };
  for (const sf of files) {
    const collect = (ranges: ts.CommentRange[] | undefined) => {
      for (const range of ranges ?? []) {
        const raw = sf.text.slice(range.pos, range.end);
        if (!/^\/\/\s*observes:/.test(raw) || comments.has(key(sf, range.pos)))
          continue;
        const parsed = /^\/\/\s*observes:\s*(\S+?)#(\S+)(?:\s+(.+?))?\s*$/.exec(
          raw,
        );
        const suffix = parsed?.[3]?.split(/(?:^|\s+)via\s+/);
        const target = parsed
          ? {
              file: parsed[1],
              function: parsed[2],
              snippet: suffix?.[0]?.trim() || undefined,
              via: suffix?.slice(1).join(" via ").trim() || undefined,
            }
          : undefined;
        const validPath =
          target &&
          !/[\\:]/.test(target.file) &&
          target.file
            .split("/")
            .every((part) => part && part !== "." && part !== "..");
        const candidates = validPath
          ? sites
              .filter(
                (s) =>
                  s.file === target.file &&
                  (s.owner === target.function || s.fn === target.function) &&
                  (!target.snippet || s.text.includes(target.snippet)),
              )
              .map((s) => s.id)
          : [];
        comments.set(key(sf, range.pos), {
          id: location(sf, range.pos),
          where: location(sf, range.pos),
          raw,
          target,
          candidateSites: candidates,
          issue: !target
            ? "invalid-syntax"
            : !validPath
              ? "invalid-target-path"
              : !candidates.length
                ? "target-not-in-inventory"
                : candidates.length > 1
                  ? "ambiguous-target"
                  : undefined,
          attachments: [],
        });
      }
    };
    const visit = (node: ts.Node) => {
      collect(compiler.getLeadingCommentRanges(sf.text, node.getFullStart()));
      collect(compiler.getTrailingCommentRanges(sf.text, node.getEnd()));
      compiler.forEachChild(node, visit);
    };
    visit(sf);
  }
  return {
    register(
      node: ts.CallExpression,
      method: string,
      testKey: string,
      inert: boolean,
    ) {
      let statement: ts.Node = node;
      while (statement.parent && !compiler.isStatement(statement))
        statement = statement.parent;
      // A comment on an if/block/declaration is not a claim about every assertion inside it.
      if (!compiler.isExpressionStatement(statement)) return;
      const sf = node.getSourceFile();
      for (const range of compiler.getLeadingCommentRanges(
        sf.text,
        statement.getFullStart(),
      ) ?? []) {
        const comment = comments.get(key(sf, range.pos));
        if (!comment) continue;
        const attachment = {
          testKey,
          source: location(sf, node.getStart(sf)),
          method,
          inert,
        };
        if (
          !comment.attachments.some(
            (a) =>
              a.testKey === testKey &&
              a.source === attachment.source &&
              a.method === method,
          )
        )
          comment.attachments.push(attachment);
      }
    },
    finish(
      bindings: { testKey: string; id: string; phases?: AssertionPhase[] }[],
    ): PragmaHint[] {
      return [...comments.values()].flatMap<PragmaHint>(
        ({ attachments, ...comment }) => {
          const sources = new Set(
            attachments.map((a) => `${a.source}#${a.method}`),
          );
          const issue =
            comment.issue ??
            (sources.size > 1
              ? "ambiguous-assertion"
              : !sources.size
                ? "unattached-assertion"
                : undefined);
          const matched = bindings.flatMap((b) =>
            attachments
              .filter((a) => a.testKey === b.testKey && !a.inert)
              .map((a) => ({ a, b })),
          );
          if (sources.size !== 1 || !matched.length)
            return [
              {
                ...comment,
                issue: issue ?? "no-owning-passed-test",
                witness: "unavailable" as const,
              },
            ];
          return matched.map(({ a, b }) => {
            const witnessIssue = assertionWitnessIssue(
              b.phases,
              a.source,
              a.method,
            );
            return {
              ...comment,
              id: `${comment.id}@${b.id}`,
              issue,
              test: b.id,
              assertionSource: a.source,
              assertionMethod: a.method,
              witness: witnessIssue
                ? ("unavailable" as const)
                : ("passed" as const),
              witnessIssue,
            };
          });
        },
      );
    },
  };
}
