/** Post-run adapter. Rust validates archive framing, freshness and record schemas first. */
import type ts from "typescript";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeWithFrontend } from "./analyze.js";
import { analysisPath } from "./compiler.js";
import { createFrontend, type CompilerFrontend } from "./frontend.js";
import type { Site } from "./types.js";

export const PROTOCOL = {
  abi: 1,
  factsSchema: 1,
  rules: "source-linked-v3/archive-3",
  capabilities: [
    "requiresTotal-v1",
    "assertion-witness-issues-v1",
    "assertion-hints-v1",
  ],
};
type Location = {
  file: string;
  line: number;
  column: number;
  source: string;
  id: string;
};
type Point = Location & { kind: string };
type Decision = Location & { conditions: string[] };
type Event = {
  type: string;
  id: string;
  phaseId?: string;
  statementId?: string;
};
type Snapshot = {
  hits?: string[];
  events?: Event[];
  decisions?: {
    meta: Decision;
    vectors: { outcome: boolean; values: (boolean | null)[] }[];
  }[];
};
type Scope = {
  version: number;
  runId: string;
  workerId: string;
  testId: string;
  testKey: string;
  retry: number;
  attemptId: string;
};
type ServerRecord = {
  type: string;
  id?: string;
  meta?: Decision;
  vector?: { outcome: boolean; values: (boolean | null)[] };
  phaseId?: string;
  statementId?: string;
  scope?: Scope;
};
function sameScope(a?: Scope, b?: Scope): boolean {
  return (
    !!a &&
    !!b &&
    a.version === b.version &&
    a.runId === b.runId &&
    a.workerId === b.workerId &&
    a.testId === b.testId &&
    a.testKey === b.testKey &&
    a.retry === b.retry &&
    a.attemptId === b.attemptId
  );
}
function serverSnapshot(records: ServerRecord[], scope?: Scope): Snapshot {
  const hits: string[] = [],
    events: Event[] = [];
  const decisions: NonNullable<Snapshot["decisions"]> = [];
  for (const record of records) {
    if (!sameScope(record.scope, scope)) continue;
    const id = record.type === "decision" ? record.meta?.id : record.id;
    if (!id) throw new Error("Missing scoped server event identity");
    if (record.type === "decision" && record.meta && record.vector)
      decisions.push({ meta: record.meta, vectors: [record.vector] });
    else if (record.type === "hit") hits.push(id);
    else throw new Error("Invalid scoped server event");
    events.push({
      type: record.type,
      id,
      phaseId: record.phaseId,
      statementId: record.statementId,
    });
  }
  return { hits, events, decisions };
}
type RecordData = {
  test: string;
  title?: string;
  testFile?: string;
  testId?: string;
  retry?: number;
  role: string;
  status?: string;
  expectedStatus?: string;
  flaky?: boolean;
  scope?: Scope;
  runtime: Snapshot[];
  browser: Snapshot[];
  server: ServerRecord[];
  phases: {
    id: string;
    kind: string;
    operation: string;
    source?: string;
    status?: string;
    causedByPhaseId?: string;
  }[];
};
export interface ArchiveInput {
  protocol: typeof PROTOCOL;
  projectRoot: string;
  runId: string;
  sourceFiles: string[];
  effects: (Omit<Site, "kind" | "fn" | "pos" | "endPos"> & {
    function: string;
  })[];
  manifest: {
    points: Point[];
    decisions: Decision[];
    branches: (Location & { alternatives: { id: string }[] })[];
  };
  records: RecordData[];
  limitations: string[];
}

export function analyzeArchive(
  input: ArchiveInput,
  suppliedCompiler?: typeof ts,
) {
  const frontend = createFrontend(input.projectRoot, suppliedCompiler);
  try {
    return analyzeArchiveWithFrontend(input, frontend);
  } finally {
    frontend.close();
  }
}

function analyzeArchiveWithFrontend(
  input: ArchiveInput,
  frontend: CompilerFrontend,
) {
  const compiler = frontend.syntax;
  if (
    input.protocol.abi !== PROTOCOL.abi ||
    input.protocol.factsSchema !== PROTOCOL.factsSchema ||
    input.protocol.rules !== PROTOCOL.rules ||
    JSON.stringify(input.protocol.capabilities) !==
      JSON.stringify(PROTOCOL.capabilities)
  )
    throw new Error(
      "Unsupported assertion analyzer protocol/rule revision/capabilities",
    );
  const root = analysisPath(input.projectRoot);
  const sources = new Map(
    input.sourceFiles.map((file) => [
      file,
      frontend.parseSource(file, readFileSync(resolve(root, file), "utf8")),
    ]),
  );
  function offset(file: string, p: { line: number; column: number }) {
    const sf = sources.get(file);
    if (!sf || p.line < 1 || p.column < 1)
      throw new Error(`Invalid site position in ${file}`);
    const n = sf.getPositionOfLineAndCharacter(p.line - 1, p.column - 1);
    if (n > sf.text.length) throw new Error(`Site outside source: ${file}`);
    return n;
  }
  const sites: Site[] = input.effects.map((e) => ({
    ...e,
    kind: "effect",
    fn: e.function,
    pos: offset(e.file, e.start),
    endPos: offset(e.file, e.end),
  }));
  // Decision atoms retain their archived decision id and index. Logical-value operands are not
  // fabricated from truthiness: in particular ?? does not establish the truthiness of its left side.
  for (const d of input.manifest.decisions) {
    const sf = sources.get(d.file);
    if (!sf) continue;
    const start = offset(d.file, d);
    let expression: ts.Node | undefined;
    function visit(n: ts.Node) {
      if (n.getStart(sf!) === start && n.getText(sf!) === d.source)
        expression ??= n;
      compiler.forEachChild(n, visit);
    }
    visit(sf);
    if (!expression)
      throw new Error(`Archived decision no longer matches source: ${d.id}`);
    const atoms: ts.Node[] = [];
    function flatten(n: ts.Node) {
      if (compiler.isParenthesizedExpression(n)) flatten(n.expression);
      else if (
        compiler.isBinaryExpression(n) &&
        [
          compiler.SyntaxKind.AmpersandAmpersandToken,
          compiler.SyntaxKind.BarBarToken,
        ].includes(n.operatorToken.kind)
      ) {
        flatten(n.left);
        flatten(n.right);
      } else atoms.push(n);
    }
    flatten(expression);
    if (atoms.length !== d.conditions.length)
      throw new Error(`Unsupported decision atom layout: ${d.id}`);
    atoms.forEach((n, i) => {
      const begin = sf.getLineAndCharacterOfPosition(n.getStart(sf)),
        end = sf.getLineAndCharacterOfPosition(n.getEnd());
      // The flow pass resolves the actual AST owner; this fallback is only for presentation.
      let parent: ts.Node | undefined = n.parent,
        owner = "<module>";
      while (parent) {
        if (compiler.isFunctionDeclaration(parent) && parent.name) {
          owner = parent.name.text;
          break;
        }
        parent = parent.parent;
      }
      sites.push({
        id: `${d.id}#${i}`,
        file: d.file,
        kind: "decision",
        category: "condition",
        classification: "contractual",
        start: { line: begin.line + 1, column: begin.character + 1 },
        end: { line: end.line + 1, column: end.character + 1 },
        pos: n.getStart(sf),
        endPos: n.getEnd(),
        text: n.getText(sf),
        fn: owner,
        owner,
        exported: false,
      });
    });
  }
  const files: Record<string, string> = {
    "inventory.json": JSON.stringify({ sites }),
  };
  const points = new Map(input.manifest.points.map((p) => [p.id, p]));
  const decisions = new Map(input.manifest.decisions.map((d) => [d.id, d]));
  const locations = new Map<string, { file: string; line: number }>(points);
  for (const b of input.manifest.branches)
    for (const a of b.alternatives) locations.set(a.id, b);
  const key = (p: { file: string; line: number; column: number }) =>
    `${p.file}:${p.line}:${p.column}`;
  const index: object[] = [];
  const executionLinks: object[] = [];
  const attempts: object[] = [];
  const limitations = new Set(input.limitations);
  limitations.add(
    "Experimental candidate analysis: no site has a checked semantic proof; no assertion percentage is reported.",
  );
  limitations.add(
    "Phase/statement execution is not data dependence. Candidate flow rules remain unverified; helper-name contracts and temporal value inference are disabled.",
  );
  limitations.add(
    "Logical-value operand sites and type-dependent effect classifications are not yet a complete denominator.",
  );
  const accepted = input.records.filter(
    (r) =>
      r.role === "test" &&
      r.status === "passed" &&
      !r.flaky &&
      r.expectedStatus !== "failed" &&
      r.scope &&
      r.scope.runId === input.runId &&
      // Keep uncertain/retried/multiple records out instead of mixing their observations.
      (r.retry ?? 0) === 0 &&
      r.scope.retry === 0 &&
      input.records.filter(
        (other) =>
          other.role === "test" &&
          (other.testId ?? other.test) === (r.testId ?? r.test),
      ).length === 1,
  );
  if (!accepted.length)
    throw new Error(
      "No uniquely attributed, passed, non-retried test attempts in archive",
    );
  for (const [i, r] of accepted.entries()) {
    const id = `A${i + 1}`;
    if (!r.testFile) {
      limitations.add("A passed test has no source file.");
      continue;
    }
    if (r.browser.length)
      limitations.add(
        "Browser evidence is not joined in this archive adapter.",
      );
    if (r.server.some((record) => !sameScope(record.scope, r.scope)))
      limitations.add(
        "Server events with missing or foreign attempt scopes were excluded.",
      );
    const snapshots = [...r.runtime, serverSnapshot(r.server, r.scope)];
    const marked = new Map<string, Set<number>>();
    function mark(p?: { file: string; line: number }) {
      if (!p || !sources.has(p.file)) return;
      if (!marked.has(p.file)) marked.set(p.file, new Set());
      marked.get(p.file)!.add(p.line);
    }
    const outcomes: Record<string, number[]> = {};
    const events = snapshots.flatMap((s) => s.events ?? []);
    for (const snap of snapshots) {
      for (const h of snap.hits ?? []) {
        if (!locations.has(h)) throw new Error(`Unknown archived hit ${h}`);
        mark(locations.get(h));
      }
      for (const d of snap.decisions ?? []) {
        const meta = decisions.get(d.meta.id);
        if (!meta) throw new Error(`Unknown archived decision ${d.meta.id}`);
        mark(meta);
        for (const v of d.vectors) {
          (outcomes[`${key(meta)}#d`] ??= [0, 0])[v.outcome ? 0 : 1] = 1;
          v.values.forEach((value, n) => {
            if (typeof value === "boolean")
              (outcomes[`${key(meta)}#${n}`] ??= [0, 0])[value ? 0 : 1] = 1;
          });
        }
      }
    }
    function describe(events: Event[]) {
      return {
        fns: [
          ...new Set(
            events
              .filter(
                (e) =>
                  e.type === "hit" && points.get(e.id)?.kind === "function",
              )
              .map((e) => key(points.get(e.id)!)),
          ),
        ],
        decs: [
          ...new Set(
            events
              .filter((e) => e.type === "decision" && decisions.has(e.id))
              .map((e) => key(decisions.get(e.id)!)),
          ),
        ],
        stmts: events.filter(
          (e) => e.type === "hit" && points.get(e.id)?.kind === "statement",
        ).length,
      };
    }
    const phases = r.phases
      .filter((p) => p.kind === "assertion" && p.source)
      .map((p) => {
        const evs = events.filter((e) => e.phaseId === p.id);
        for (const e of p.status === "passed" ? evs : [])
          executionLinks.push({
            attempt: id,
            point: e.id,
            location:
              points.get(e.id) ?? decisions.get(e.id) ?? locations.get(e.id),
            phase: p.id,
            statement: e.statementId,
            operation: p.operation,
            assertionSource: p.source,
            meaning: "execution-only",
          });
        return {
          op: p.operation,
          source: p.source!,
          status: p.status,
          ...describe(evs),
        };
      });
    const statements: Record<string, ReturnType<typeof describe>> = {};
    for (const e of events)
      if (e.statementId && !statements[e.statementId])
        statements[e.statementId] = describe(
          events.filter((other) => other.statementId === e.statementId),
        );
    const phaseLines = phases
      .map((p) => /^(.*):(\d+):(\d+)$/.exec(p.source))
      .filter((m) => m && m[1] === r.testFile)
      .map((m) => Number(m![2]));
    index.push({
      id,
      name: r.test,
      title: r.title ?? r.test,
      file: r.testFile,
      line: 0,
      ok: true,
      phaseLines,
    });
    attempts.push({
      id,
      testId: r.testId,
      name: r.test,
      file: r.testFile,
      retry: r.retry ?? 0,
    });
    files[`cov/${id}.lcov`] = [...marked]
      .map(
        ([file, lines]) =>
          `SF:${file}\n${[...lines].map((l) => `DA:${l},1`).join("\n")}\nend_of_record\n`,
      )
      .join("");
    files[`cov/${id}.outcomes.json`] = JSON.stringify(outcomes);
    files[`cov/${id}.phases.json`] = JSON.stringify(phases);
    files[`cov/${id}.statements.json`] = JSON.stringify(statements);
  }
  if (accepted.length !== input.records.filter((r) => r.role === "test").length)
    limitations.add(
      "Failed, flaky, retried, duplicate or unattributed test records were excluded; this is not whole-suite assertion coverage.",
    );
  files["cov/index.json"] = JSON.stringify(index);
  const result = analyzeWithFrontend(
    {
      projectRoot: root,
      evidenceFiles: files,
      sourceFiles: input.sourceFiles,
      testFiles: [
        ...new Set(accepted.flatMap((r) => (r.testFile ? [r.testFile] : []))),
      ],
    },
    frontend,
  );
  return {
    protocol: PROTOCOL,
    ...result,
    inventory: sites,
    executionLinks,
    attempts,
    limitations: [...limitations, ...frontend.limitations].sort(),
  };
}
