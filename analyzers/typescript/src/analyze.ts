/**
 * Source observation and value-flow analysis for ordinary archive queries.
 * Inputs are query-local evidence and matching project sources. Known inference
 * limits remain explicit; these facts are not a soundness or safety proof.
 */
import type ts from "typescript";
import { relative as pathRelative, resolve } from "node:path";
import { analysisPath } from "./compiler.js";
import { createFrontend, type CompilerFrontend } from "./frontend.js";
import { assertionWitnessIssue, collectPragmas } from "./pragmas.js";
import { analyzeMockCounts, type MockCountEvidence } from "./mock-counts.js";
import type { AnalyzeOptions, Site } from "./types.js";
export type { AnalyzeOptions, Site } from "./types.js";

// Archive/site/test identities use forward slashes on every host.
const relative = (from: string, to: string) =>
  pathRelative(from, to).replaceAll("\\", "/");

export interface Boundary {
  boundary: string;
  facet?: string;
  via?: string;
}

export interface SinkBinding {
  sink: string;
  prodName: string;
  param: string;
  member?: string;
}

export function analyze(options: AnalyzeOptions) {
  if (
    !options ||
    typeof options.projectRoot !== "string" ||
    !options.evidenceFiles
  )
    throw new Error("projectRoot and in-memory evidenceFiles are required");
  const frontend = createFrontend(options.projectRoot, options.typescript);
  try {
    return analyzeWithFrontend(options, frontend);
  } finally {
    frontend.close();
  }
}

/** Internal archive entry: one compiler session, closed by the archive caller. */
export function analyzeWithFrontend(
  options: AnalyzeOptions,
  frontend: CompilerFrontend,
) {
  const root = analysisPath(options.projectRoot);
  const ts = frontend.syntax;
  const srcDir = (options.sourceDir ?? "src").replace(/\/$/, "");
  const testDir = (options.testDir ?? "tests").replace(/\/$/, "");
  const evidence = options.evidenceFiles;
  const readEvidence = (p: string): string => {
    const value = evidence[p];
    if (value === undefined)
      throw new Error(`Missing archive evidence input: ${p}`);
    return value;
  };
  const hasEvidence = (p: string) => Object.hasOwn(evidence, p);
  const inventory = JSON.parse(readEvidence("inventory.json")) as {
    sites: Site[];
  };
  if (!Array.isArray(inventory.sites))
    throw new Error("inventory.json must contain a sites array");
  const sites = inventory.sites;
  type Strength = "presence" | "value" | "total";
  const RANK: Record<Strength, number> = { presence: 1, value: 2, total: 3 };
  const stronger = (
    a: Strength | undefined,
    b: Strength | undefined,
  ): Strength | undefined => (!a ? b : !b ? a : RANK[a] >= RANK[b] ? a : b);

  const effectSites = sites.filter((s) => s.kind === "effect");
  const siteById = new Map(sites.map((s) => [s.id, s]));

  interface RuntimeTest {
    id: string;
    name: string;
    file: string;
    line: number;
    ok: boolean;
    /** supercov runs carry no test line: the leaf title and the lines of the test's assertion phases link it instead */
    title?: string;
    phaseLines?: number[];
  }
  const runtimeTests = (
    JSON.parse(readEvidence("cov/index.json")) as RuntimeTest[]
  ).filter((t) => t.ok);

  const coverage = new Map<string, Map<string, Set<number>>>();
  for (const t of runtimeTests) {
    const perFile = new Map<string, Set<number>>();
    let current: Set<number> | undefined;
    let currentFile = "";
    for (const line of readEvidence(`cov/${t.id}.lcov`).split("\n")) {
      if (line.startsWith("SF:")) {
        const f = line.slice(3).trim();
        currentFile = f.startsWith("/") ? relative(root, f) : f;
        current = new Set();
        perFile.set(currentFile, current);
      } else if (line.startsWith("DA:") && current) {
        const [ln, hits] = line.slice(3).split(",").map(Number);
        if (hits <= 0) continue;
        current.add(ln);
      }
    }
    coverage.set(t.id, perFile);
  }
  /**
   * Optional per-test condition outcomes: cov/<test>.outcomes.json maps "<file>:<line>:<column>" of a
   * condition atom to [timesTrue, timesFalse] within that test. supercov's MC/DC runtime provides this;
   * without the file, branch outcomes are inferred from line coverage as before.
   */
  const outcomes = new Map<string, Map<string, [number, number]>>();
  for (const t of runtimeTests) {
    const p = `cov/${t.id}.outcomes.json`;
    if (!hasEvidence(p)) continue;
    const raw = JSON.parse(readEvidence(p)) as Record<string, [number, number]>;
    outcomes.set(t.id, new Map(Object.entries(raw)));
  }
  /** supercov input: statement/function points rather than executed lines (see covers()). */
  const statementGranular = outcomes.size > 0;
  /**
   * Assertion phases from Supercov (cov/<test>.phases.json). Exact call location,
   * method and successful status witness a static observation; function entries
   * are execution evidence only, never proof that a return value was checked.
   */
  interface RuntimePhase {
    op: string;
    source: string;
    status?: string;
    fns: string[];
    /** functions entered in the browser during the causing action (Playwright): a page render enters every
     *  component, so these only say the component rendered (presence) */
    browserFns?: string[];
    stmts: number;
    decs: string[];
    causedBy?: string;
  }
  const runtimePhases = new Map<string, RuntimePhase[]>();
  /** cov/<test>.statements.json: production function entries recorded while each test-file statement ran. */
  interface StatementAttribution {
    fns: string[];
    decs: string[];
    stmts: number;
  }
  const runtimeStatements = new Map<
    string,
    Record<string, StatementAttribution>
  >();
  for (const t of runtimeTests) {
    const p = `cov/${t.id}.phases.json`;
    if (hasEvidence(p))
      runtimePhases.set(t.id, JSON.parse(readEvidence(p)) as RuntimePhase[]);
    const sp = `cov/${t.id}.statements.json`;
    if (hasEvidence(sp)) {
      const parsed = JSON.parse(readEvidence(sp)) as Record<
        string,
        StatementAttribution
      >;
      if (Object.keys(parsed).length) runtimeStatements.set(t.id, parsed);
    }
  }
  /**
   * Outcome key of a decision atom: "<file>:<line>:<column>#<index>", where the position is the start
   * of the whole condition expression of the if/ternary/loop and the index is the atom's place among
   * the expression's leaf conditions in source order (supercov's `conditions[]` order). Value-position
   * logical expressions have no decision outcome record and yield undefined.
   */
  const outcomeKeyCache = new Map<string, string | null>();
  function outcomeKeyOf(s: Site): string | undefined {
    const cached = outcomeKeyCache.get(s.id);
    if (cached !== undefined) return cached ?? undefined;
    const compute = (): string | undefined => {
      const atom = siteNodes.get(s.id);
      if (!atom) return undefined;
      const sf = atom.getSourceFile();
      const isLogicalBinary = (x: ts.Node): x is ts.BinaryExpression =>
        ts.isBinaryExpression(x) &&
        [
          ts.SyntaxKind.AmpersandAmpersandToken,
          ts.SyntaxKind.BarBarToken,
          ts.SyntaxKind.QuestionQuestionToken,
        ].includes(x.operatorToken.kind);
      let top: ts.Node = atom;
      while (
        top.parent &&
        (isLogicalBinary(top.parent) ||
          ts.isParenthesizedExpression(top.parent))
      )
        top = top.parent;
      const ctx = top.parent;
      const isCondition =
        ((ts.isIfStatement(ctx) ||
          ts.isWhileStatement(ctx) ||
          ts.isDoStatement(ctx)) &&
          ctx.expression === top) ||
        (ts.isConditionalExpression(ctx) && ctx.condition === top) ||
        (ts.isForStatement(ctx) && ctx.condition === top);
      if (!isCondition) {
        // value-position `a && b` / `a || b` / `a ?? b`: supercov records per logical-value branch which
        // side was selected and whether the result was truthy, from which the converter derives both
        // operands' outcomes. The branch is the innermost logical expression whose direct operand is the
        // atom; the key carries its start and end so nested chains (`a && b && c`) stay distinct.
        let b: ts.Node | undefined = atom.parent;
        while (b && ts.isParenthesizedExpression(b)) b = b.parent;
        if (!b || !isLogicalBinary(b)) return undefined;
        const same = (x: ts.Expression) =>
          unwrap(x).getStart(sf) === atom.getStart(sf) &&
          unwrap(x).getEnd() === atom.getEnd();
        const index = same(b.left) ? 0 : same(b.right) ? 1 : -1;
        if (index < 0) return undefined;
        const start = sf.getLineAndCharacterOfPosition(b.getStart(sf));
        const end = sf.getLineAndCharacterOfPosition(b.getEnd());
        return `${s.file}:${start.line + 1}:${start.character + 1}~${end.line + 1}:${end.character + 1}#${index}|2`;
      }
      const leaves: ts.Node[] = [];
      const collect = (x: ts.Node) => {
        if (ts.isParenthesizedExpression(x)) return collect(x.expression);
        if (isLogicalBinary(x)) {
          collect(x.left);
          collect(x.right);
          return;
        }
        leaves.push(x);
      };
      collect(top);
      const index = leaves.findIndex(
        (l) =>
          l.getStart(sf) === atom.getStart(sf) && l.getEnd() === atom.getEnd(),
      );
      if (index < 0) return undefined;
      const { line, character } = sf.getLineAndCharacterOfPosition(
        top.getStart(sf),
      );
      return `${s.file}:${line + 1}:${character + 1}#${index}|${leaves.length}`;
    };
    const key = compute();
    outcomeKeyCache.set(s.id, key ?? null);
    return key;
  }
  /** Tests in which the condition at `s` took `outcome`; undefined when no test carries outcome data for it. */
  /**
   * Value-position operand (`a || b`, `a && b`, `a ?? b`): the tests in which this operand's value was the
   * value of the whole expression. A mutation of the operand's value is observable only there, which is
   * the question for a value-position atom (the MC/DC stuck-outcome question is the one for conditions).
   */
  function testsWhereSelected(s: Site): Set<string> | undefined {
    const full = outcomeKeyOf(s);
    if (!full || !full.includes("~")) return undefined;
    const [key] = full.split("|");
    const selKey = key.replace(/#(\d+)$/, "#s$1");
    let any = false;
    const res = new Set<string>();
    for (const [tid, m] of outcomes) {
      const o = m.get(selKey);
      if (!o) continue;
      any = true;
      if (o[0] > 0) res.add(tid);
    }
    return any ? res : undefined;
  }
  function testsWithOutcome(
    s: Site,
    outcome: boolean,
  ): Set<string> | undefined {
    const full = outcomeKeyOf(s);
    if (!full) return undefined;
    const [key, leafCount] = full.split("|");
    const decisionKey = key.slice(0, key.lastIndexOf("#"));
    let any = false;
    const res = new Set<string>();
    for (const [tid, m] of outcomes) {
      const o = m.get(key);
      if (!o) continue;
      // the atom split must agree with supercov's condition list, else the index means nothing
      let n = 0;
      while (m.has(`${decisionKey}#${n}`)) n++;
      if (n !== Number(leafCount)) return undefined;
      any = true;
      if ((outcome ? o[0] : o[1]) > 0) res.add(tid);
    }
    return any ? res : undefined;
  }
  /** Module-setup coverage (imports, module-level statements) of the test's file: counts for module-level sites only. */
  const setupCoverage = new Map<string, Map<string, Set<number>>>();
  for (const t of runtimeTests) {
    const p = `cov/${t.id}.setup.lcov`;
    if (!hasEvidence(p)) continue;
    const perFile = new Map<string, Set<number>>();
    let current: Set<number> | undefined;
    for (const line of readEvidence(p).split("\n")) {
      if (line.startsWith("SF:")) {
        current = new Set();
        perFile.set(line.slice(3).trim(), current);
      } else if (line.startsWith("DA:") && current)
        current.add(Number(line.slice(3).split(",")[0]));
    }
    setupCoverage.set(t.id, perFile);
  }
  function covers(testId: string, s: Site): boolean {
    const lines = coverage.get(testId)?.get(s.file);
    if (s.owner === "<module>") {
      const setup = setupCoverage.get(testId)?.get(s.file);
      if (setup)
        for (let l = s.start.line; l <= s.end.line; l++)
          if (setup.has(l)) return true;
    }
    if (!lines) return false;
    for (let l = s.start.line; l <= s.end.line; l++)
      if (lines.has(l)) return true;
    // Statement-granular coverage (supercov marks a statement's first line only): an expression site
    // inside a multi-line statement is covered when its own statement is. A site that is itself a
    // statement (return, throw, expression statement) has its own line marked, so no fallback; the climb
    // stops at the first statement and never crosses a block or a function boundary, so a branch that
    // did not run is not credited with its parent `if`. Line-granular data (v8 lcov) marks every line of
    // an executed statement and the definition line of every function, so the fallback is wrong there.
    if (!statementGranular) return false;
    const node = siteNodes.get(s.id);
    if (!node || ts.isStatement(node) || ts.isBlock(node)) return false;
    const sf = node.getSourceFile();
    const lineOf = (x: ts.Node) =>
      sf.getLineAndCharacterOfPosition(x.getStart(sf)).line + 1;
    let n: ts.Node | undefined = node.parent;
    while (n && !ts.isStatement(n) && !ts.isSourceFile(n)) {
      // an expression-bodied arrow (`x => ({ label })`): its entry is a function point on the arrow's line
      if (ts.isFunctionLike(n)) return lines.has(lineOf(n));
      if (ts.isBlock(n)) return false;
      n = n.parent;
    }
    if (!n || ts.isSourceFile(n) || ts.isBlock(n)) return false;
    return lines.has(lineOf(n));
  }

  // ---------------------------------------------------------------------------
  // Program over src + tests
  // ---------------------------------------------------------------------------
  const program = frontend.openProgram(options);
  const checker = program.checker;
  const allFiles = program.files.filter(
    (f) => !f.isDeclarationFile && !f.fileName.includes("/node_modules/"),
  );
  const rel = (sf: ts.SourceFile) => relative(root, sf.fileName);
  const isProdFile = (sf: ts.SourceFile) =>
    options.sourceFiles
      ? options.sourceFiles.includes(rel(sf))
      : rel(sf).startsWith(srcDir + "/");
  const isTestFile = (sf: ts.SourceFile) =>
    options.testFiles
      ? options.testFiles.includes(rel(sf))
      : rel(sf).startsWith(testDir + "/") &&
        /\.(test|spec)\.(ts|tsx|mts)$/.test(sf.fileName);
  const srcByFile = new Map(
    allFiles.filter(isProdFile).map((sf) => [rel(sf), sf]),
  );
  const isExternalDecl = (d: ts.Declaration) =>
    d.getSourceFile().fileName.includes("/node_modules/") ||
    d.getSourceFile().isDeclarationFile;

  function unwrap(e: ts.Expression): ts.Expression {
    let cur = e;
    for (;;) {
      if (
        ts.isParenthesizedExpression(cur) ||
        ts.isAwaitExpression(cur) ||
        ts.isNonNullExpression(cur) ||
        ts.isAsExpression(cur) ||
        ts.isTypeAssertionExpression(cur)
      ) {
        cur = cur.expression;
        continue;
      }
      return cur;
    }
  }
  function symbolOf(id: ts.Node): ts.Symbol | undefined {
    // `{ logger }` shorthand: the name resolves to the property; we want the value it refers to
    let s =
      ts.isIdentifier(id) &&
      ts.isShorthandPropertyAssignment(id.parent) &&
      id.parent.name === id
        ? checker.getShorthandAssignmentValueSymbol(id.parent)
        : checker.getSymbolAtLocation(id);
    if (s && s.flags & ts.SymbolFlags.Alias) {
      try {
        s = checker.getAliasedSymbol(s);
      } catch {
        /* unresolvable */
      }
    }
    return s;
  }
  function declOf(id: ts.Node): ts.Declaration | undefined {
    const s = symbolOf(id);
    return s?.valueDeclaration ?? s?.declarations?.[0];
  }
  function enclosingFunction(
    node: ts.Node,
  ): ts.SignatureDeclaration | undefined {
    let n: ts.Node | undefined = node.parent;
    while (n) {
      if (ts.isFunctionLike(n)) return n;
      n = n.parent;
    }
    return undefined;
  }
  function nameOfFunction(fn: ts.Node | undefined): string | undefined {
    if (!fn) return undefined;
    if (ts.isFunctionDeclaration(fn) && fn.name) return fn.name.text;
    if (
      (ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) &&
      ts.isVariableDeclaration(fn.parent) &&
      ts.isIdentifier(fn.parent.name)
    )
      return fn.parent.name.text;
    if (ts.isClassDeclaration(fn) && fn.name) return fn.name.text;
    if (
      ts.isMethodDeclaration(fn) &&
      ts.isIdentifier(fn.name) &&
      ts.isClassDeclaration(fn.parent) &&
      fn.parent.name
    )
      return `${fn.parent.name.text}.${fn.name.text}`;
    if (
      ts.isConstructorDeclaration(fn) &&
      ts.isClassDeclaration(fn.parent) &&
      fn.parent.name
    )
      return `${fn.parent.name.text}.constructor`;
    return undefined;
  }
  /** Nearest named function (skipping anonymous callbacks), matching inventory's `owner`. */
  function ownerOf(node: ts.Node): ts.SignatureDeclaration | undefined {
    let fn = enclosingFunction(node);
    while (fn && !nameOfFunction(fn)) fn = enclosingFunction(fn);
    return fn;
  }
  function siteNode(s: Site): ts.Node | undefined {
    const sf = srcByFile.get(s.file);
    if (!sf) return undefined;
    let found: ts.Node | undefined;
    const visit = (n: ts.Node) => {
      // prefer the deepest node with this exact range (a statement and its call expression share it)
      if (n.getStart(sf) === s.pos && n.getEnd() === s.endPos) found = n;
      if (n.getStart(sf) <= s.pos && n.getEnd() >= s.endPos)
        ts.forEachChild(n, visit);
    };
    visit(sf);
    return found;
  }
  const siteNodes = new Map<string, ts.Node>();
  for (const s of sites) {
    const n = siteNode(s);
    if (n) siteNodes.set(s.id, n);
  }
  function smallestSiteContaining(
    file: string,
    start: number,
    end: number,
    onlyEffects = true,
  ): Site | undefined {
    return (onlyEffects ? effectSites : sites)
      .filter((s) => s.file === file && s.pos <= start && s.endPos >= end)
      .sort((a, b) => a.endPos - a.pos - (b.endPos - b.pos))[0];
  }

  // ---------------------------------------------------------------------------
  // Boundaries
  // ---------------------------------------------------------------------------
  interface Observation extends Boundary {
    strength: Strength;
    where: string;
    /** Exact authored call location, not a line/title approximation. */
    assertionSource?: string;
    assertionMethod?: string;
    implicit?: boolean;
    pattern?: string;
    literal?: string;
    fragment?: string;
    negative?: boolean;
    /** derived from supercov's assertion phases (production functions entered while `expect(...)` was evaluated) */
    runtime?: boolean;
    /** a whole-page render witnessed by a DOM assertion somewhere on the page: says the function ran, not that
     *  its output was read, so it never predicts that removing the function would be caught */
    weak?: boolean;
    /** the assertion pins the sink's whole call list (a call count, or `mock.calls` compared as a whole or
     *  through a projection): it witnesses both that the pinned calls happened and that no other call did */
    callList?: boolean;
    /** Source projection of a Node mock, not proof of production-site dependence. */
    mock?: MockProjection;
    /** Relation between both operands; strength alone cannot establish a value check. */
    comparison?: Comparison;
  }
  interface ComparisonOperand {
    source: string;
    binding?: string;
  }
  interface Comparison {
    predicate: string;
    actual: ComparisonOperand;
    expected: ComparisonOperand;
    relation: "same-immutable-binding" | "unresolved";
  }
  interface MockProjection {
    target: string;
    kind: "call-count" | "call-arguments" | "call-history" | "projection";
    path: string[];
    countEvidence?: MockCountEvidence;
  }

  /** Does a log site's message template (constant parts in order, placeholders as wildcards) fit an asserted literal? */
  function templateMatches(s: Site, literal: string): boolean {
    const raw = (s.arg0 ?? "").replace(/^[`'"]|[`'"]$/g, "");
    const parts = raw
      .split(/\$\{[^}]*\}/)
      .map((p) => p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
    try {
      return new RegExp(parts.join(".*")).test(literal);
    } catch {
      return false;
    }
  }
  /** An observation pins a log site when its pattern, full literal and/or fragment fit the site's message. */
  function messageFits(ob: Observation, s: Site): boolean {
    if (ob.pattern && !patternMatchesSite(ob.pattern, s)) return false;
    if (ob.literal && !templateMatches(s, ob.literal)) return false;
    // a fragment may be a prefix/substring of the template's constant text, or a whole message with values filled in
    if (
      ob.fragment &&
      !patternMatchesSite(
        ob.fragment.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"),
        s,
      ) &&
      !templateMatches(s, ob.fragment)
    )
      return false;
    return true;
  }

  const LOG_TEMPLATE_CACHE = new Map<string, string[]>();
  /** Constant text variants of a log call's first argument for pattern matching. */
  function logTexts(s: Site): string[] {
    const key = s.id;
    let v = LOG_TEMPLATE_CACHE.get(key);
    if (!v) {
      const raw = (s.arg0 ?? "").replace(/^[`'"]|[`'"]$/g, "");
      const variants = [
        raw.replace(/\$\{[^}]*\}/g, ""),
        raw.replace(/\$\{[^}]*\}/g, "0"),
        raw.replace(/\$\{[^}]*\}/g, "x"),
      ];
      // The logger implementation prefixes every line; a pattern may target the prefix.
      v = [...variants, ...variants.map((t) => `[supergateway] ${t}`)];
      LOG_TEMPLATE_CACHE.set(key, v);
    }
    return v;
  }
  function patternMatchesSite(pattern: string, s: Site): boolean {
    let re: RegExp;
    try {
      re = new RegExp(pattern);
    } catch {
      return false;
    }
    return logTexts(s).some((t) => re.test(t));
  }

  /** Production side: which boundaries does an effect site emit into (directly). */
  function directBoundaries(s: Site): Boundary[] {
    const chain = s.chain ?? [];
    const m = s.method;
    switch (s.category) {
      case "io-call": {
        if (m === "status" || m === "writeHead" || m === "sendStatus")
          return [{ boundary: "client-status" }];
        if (m === "setHeader")
          return [
            {
              boundary: "client-header",
              facet: /^['"`]/.test(s.arg0 ?? "")
                ? s.arg0!.slice(1, -1).toLowerCase()
                : "*",
            },
          ];
        if (m === "handleRequest")
          return [
            { boundary: "client-status" },
            { boundary: "client-header", facet: "*" },
            { boundary: "client-message" },
          ];
        if (m === "write" && chain.includes("stdin"))
          return [{ boundary: "child-stdin" }];
        if (m === "write" && chain.includes("stdout"))
          return [{ boundary: "client-message" }, { boundary: "stdout" }];
        if (m === "kill" || m === "spawn")
          return [{ boundary: "child-lifecycle" }];
        if (m === "exit") return [{ boundary: "exit" }];
        if (m === "request") return [{ boundary: "upstream" }];
        if (m === "json" || m === "send" || m === "end" || m === "write")
          return [{ boundary: "client-message" }];
        if (m === "close" || m === "terminate" || m === "destroy")
          return [{ boundary: "client-lifecycle" }];
        return [{ boundary: "lifecycle" }];
      }
      case "log":
        return [
          { boundary: m === "error" ? "stderr" : "stdout", facet: "log" },
        ];
      case "schedule":
        return [{ boundary: "timing" }];
      case "return":
      case "callback-return":
        return [{ boundary: "return:" + s.owner }];
      case "throw":
        return [{ boundary: "throw:" + s.owner }];
      case "state-write": {
        // globalThis.prisma = ...: a global a test can read back directly
        const node = siteNodes.get(s.id);
        const target =
          node && ts.isBinaryExpression(node)
            ? unwrap(node.left)
            : node && ts.isDeleteExpression(node)
              ? unwrap(node.expression)
              : undefined;
        if (
          target &&
          ts.isPropertyAccessExpression(target) &&
          ts.isIdentifier(target.expression) &&
          GLOBAL_ROOTS.has(target.expression.text)
        )
          return [{ boundary: "global:" + target.name.text }];
        return [{ boundary: "internal" }];
      }
      case "external-call": {
        // this.getSessionTable().upsert(...): a getter returning an injected field (`this.prisma[this.tableName]`)
        const viaGetter = injectedFieldReceiver(s);
        if (viaGetter) return [viaGetter];
        // prisma.article.count(...) on a parameter: boundary callback:prisma, facet article.count
        if (s.note === "this-callback" || s.note === "param")
          return [
            {
              boundary:
                "callback:" + (chain[0] === "this" ? chain[1] : chain[0]),
              facet:
                chain.slice(chain[0] === "this" ? 2 : 1).join(".") || undefined,
            },
          ];
        return [{ boundary: "internal" }];
      }
      default:
        return [{ boundary: "internal" }];
    }
  }

  /**
   * `this.getX().method(...)` where `getX()` returns `this.field`, `this.field.y` or `this.field[key]`: the call
   * writes into whatever was injected as `field` (a constructor parameter of the same name, or a parameter
   * property). Boundary callback:<field>, facet the member path with `*` for a computed key.
   */
  function injectedFieldReceiver(s: Site): Boundary | undefined {
    const node = siteNodes.get(s.id);
    if (!node || !ts.isCallExpression(node)) return undefined;
    const callee = unwrap(node.expression);
    if (!ts.isPropertyAccessExpression(callee)) return undefined;
    const recv = unwrap(callee.expression);
    if (!ts.isCallExpression(recv) || recv.arguments.length) return undefined;
    const getter = unwrap(recv.expression);
    if (
      !ts.isPropertyAccessExpression(getter) ||
      getter.expression.kind !== ts.SyntaxKind.ThisKeyword
    )
      return undefined;
    const m = checker.getSymbolAtLocation(getter.name)?.valueDeclaration;
    if (!m || !ts.isMethodDeclaration(m) || !m.body) return undefined;
    let ret: ts.Expression | undefined;
    const visit = (n: ts.Node) => {
      if (ret) return;
      if (ts.isReturnStatement(n) && n.expression) ret = n.expression;
      else if (!ts.isFunctionLike(n)) ts.forEachChild(n, visit);
    };
    visit(m.body);
    if (!ret) return undefined;
    // peel `(this.prisma as any)[this.tableName]` down to the field and the member path
    const facet: string[] = [];
    let cur: ts.Expression = unwrap(ret);
    for (let i = 0; i < 6; i++) {
      if (ts.isElementAccessExpression(cur)) {
        facet.unshift("*");
        cur = unwrap(cur.expression);
        continue;
      }
      if (
        ts.isPropertyAccessExpression(cur) &&
        cur.expression.kind !== ts.SyntaxKind.ThisKeyword
      ) {
        facet.unshift(cur.name.text);
        cur = unwrap(cur.expression);
        continue;
      }
      break;
    }
    if (
      !ts.isPropertyAccessExpression(cur) ||
      cur.expression.kind !== ts.SyntaxKind.ThisKeyword
    )
      return undefined;
    return {
      boundary: "callback:" + cur.name.text,
      facet: [...facet, callee.name.text].join("."),
      via: `${getter.name.text}()`,
    };
  }

  /** Third-party calls whose effect we model rather than analyze. */
  function externalSink(calleeName: string, path: string[]): Boundary[] {
    if (calleeName === "cors")
      return [
        { boundary: "client-header", facet: "access-control-allow-origin" },
        { boundary: "client-header", facet: "access-control-expose-headers" },
      ];
    if (calleeName === "Server") {
      if (path[0] === "version")
        return [{ boundary: "client-message", facet: "serverInfo.version" }];
      if (path[0] === "name")
        return [{ boundary: "client-message", facet: "serverInfo.name" }];
    }
    return [];
  }

  // ---------------------------------------------------------------------------
  // Inter-procedural value flow: where does a value end up?
  // ---------------------------------------------------------------------------
  interface FlowResult {
    boundaries: Boundary[];
    sites: Set<string>;
  }
  const ITERATORS = new Set([
    "forEach",
    "map",
    "filter",
    "some",
    "every",
    "find",
    "flatMap",
    "reduce",
  ]);
  const flowCache = new Map<string, FlowResult>();

  function fnKey(fn: ts.Node) {
    return `${rel(fn.getSourceFile())}:${fn.getStart()}`;
  }

  /** Project function/class declaration for a callee identifier, if any. */
  function projectCallee(
    callee: ts.Expression,
  ): ts.SignatureDeclaration | ts.ClassDeclaration | undefined {
    const c = unwrap(callee);
    // this.sessionToRow(...) / Klass.helper(...): a method of a project class
    if (ts.isPropertyAccessExpression(c)) {
      const m = checker.getSymbolAtLocation(c.name)?.valueDeclaration;
      return m && ts.isMethodDeclaration(m) && isProdFile(m.getSourceFile())
        ? m
        : undefined;
    }
    if (!ts.isIdentifier(c)) return undefined;
    const d = declOf(c);
    if (!d || !isProdFile(d.getSourceFile())) return undefined;
    if (ts.isFunctionDeclaration(d) || ts.isClassDeclaration(d)) return d;
    if (
      ts.isVariableDeclaration(d) &&
      d.initializer &&
      (ts.isArrowFunction(unwrap(d.initializer)) ||
        ts.isFunctionExpression(unwrap(d.initializer)))
    )
      return unwrap(d.initializer) as ts.SignatureDeclaration;
    return undefined;
  }
  function paramsOf(
    fn: ts.SignatureDeclaration | ts.ClassDeclaration,
  ): readonly ts.ParameterDeclaration[] {
    if (ts.isClassDeclaration(fn)) {
      const ctor = fn.members.find(ts.isConstructorDeclaration);
      return ctor?.parameters ?? [];
    }
    return fn.parameters;
  }
  function bodyOf(fn: ts.SignatureDeclaration | ts.ClassDeclaration): ts.Node {
    return fn;
  }

  /** Resolve (function, param index, object path) to the binding declaration that receives the value. */
  function bindingFor(
    fn: ts.SignatureDeclaration | ts.ClassDeclaration,
    index: number,
    path: string[],
  ): ts.Declaration | undefined {
    const p = paramsOf(fn)[index];
    if (!p) return undefined;
    if (!path.length) return p;
    if (ts.isObjectBindingPattern(p.name)) {
      const el = p.name.elements.find(
        (e) => ((e.propertyName ?? e.name) as ts.Node).getText() === path[0],
      );
      return el ?? p;
    }
    if (ts.isIdentifier(p.name)) {
      // const { x } = param   inside the body
      let found: ts.Declaration | undefined;
      const visit = (n: ts.Node) => {
        if (found) return;
        if (
          ts.isVariableDeclaration(n) &&
          n.initializer &&
          ts.isIdentifier(unwrap(n.initializer)) &&
          declOf(unwrap(n.initializer)) === p &&
          ts.isObjectBindingPattern(n.name)
        ) {
          found = n.name.elements.find(
            (e) =>
              ((e.propertyName ?? e.name) as ts.Node).getText() === path[0],
          );
        }
        ts.forEachChild(n, visit);
      };
      visit(bodyOf(fn));
      return found ?? p;
    }
    return p;
  }

  /** Follow a value node to boundaries and effect sites. */
  function propagateValue(
    value: ts.Node,
    acc: FlowResult,
    visited: Set<string>,
    depth: number,
  ) {
    if (depth > 12) return;
    const sf = value.getSourceFile();
    const file = rel(sf);
    const key = `${file}:${value.getStart(sf)}:${value.getEnd()}`;
    if (visited.has(key)) return;
    visited.add(key);

    // is the value inside an effect site? then that site carries it
    const site = smallestSiteContaining(
      file,
      value.getStart(sf),
      value.getEnd(),
    );
    if (
      site &&
      site.category !== "return" &&
      site.category !== "callback-return"
    ) {
      acc.sites.add(site.id);
      // keep climbing too: the site's own value may flow further (e.g. `res.status(x).json(y)` is fine as a site)
    }

    // climb to find how the value is consumed
    let path: string[] = [];
    let n: ts.Node = value;
    while (n.parent) {
      const p: ts.Node = n.parent;
      if (ts.isPropertyAssignment(p) && p.initializer === n) {
        path = [p.name.getText(), ...path];
        n = p;
        continue;
      }
      if (ts.isShorthandPropertyAssignment(p)) {
        path = [p.name.getText(), ...path];
        n = p;
        continue;
      }
      if (
        ts.isObjectLiteralExpression(p) ||
        ts.isParenthesizedExpression(p) ||
        ts.isAwaitExpression(p) ||
        ts.isNonNullExpression(p) ||
        ts.isAsExpression(p) ||
        ts.isSpreadAssignment(p) ||
        ts.isConditionalExpression(p) ||
        ts.isTemplateSpan(p) ||
        ts.isTemplateExpression(p) ||
        (ts.isBinaryExpression(p) &&
          !(
            p.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
            p.operatorToken.kind <= ts.SyntaxKind.LastAssignment
          ))
      ) {
        n = p;
        continue;
      }
      if (ts.isPropertyAccessExpression(p) && p.expression === n) {
        n = p;
        continue;
      }
      if (ts.isElementAccessExpression(p) && p.expression === n) {
        n = p;
        continue;
      }
      if (ts.isVariableDeclaration(p) && p.initializer === n) {
        if (ts.isIdentifier(p.name))
          propagateBinding(p, acc, visited, depth + 1);
        else if (ts.isObjectBindingPattern(p.name))
          for (const el of p.name.elements)
            propagateBinding(el, acc, visited, depth + 1);
        return;
      }
      if (
        ts.isBinaryExpression(p) &&
        p.right === n &&
        p.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        ts.isIdentifier(p.left)
      ) {
        const d = declOf(p.left);
        if (d) propagateBinding(d, acc, visited, depth + 1);
        return;
      }
      // sessionParams.accountOwner = value: the value becomes part of a local object, which flows on wherever the
      // object does (returned, handed to a constructor or factory, ...). Writes into `this`/parameters are sites.
      if (
        ts.isBinaryExpression(p) &&
        p.right === n &&
        p.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        (ts.isPropertyAccessExpression(p.left) ||
          ts.isElementAccessExpression(p.left))
      ) {
        const root = rootOfExpr(p.left);
        const d = ts.isIdentifier(root) ? declOf(root) : undefined;
        if (d && ts.isVariableDeclaration(d) && enclosingFunction(d))
          propagateBinding(d, acc, visited, depth + 1);
        return;
      }
      if (ts.isReturnStatement(p) || (ts.isArrowFunction(p) && p.body === n)) {
        // the return site itself carries the value: a test that reads the function's return observes it
        // (this is what makes a branch's assignment evident through `expect(fn()).toEqual(...)`)
        const target = ts.isArrowFunction(p) ? n : p;
        const rs = smallestSiteContaining(
          file,
          target.getStart(sf),
          target.getEnd(),
        );
        if (
          rs &&
          (rs.category === "return" || rs.category === "callback-return")
        )
          acc.sites.add(rs.id);
        // value becomes the return of the enclosing function -> flows to that function's callers
        const fn = ts.isArrowFunction(p) ? p : enclosingFunction(p);
        if (fn) {
          // callback return (map/forEach callback): value flows to the call's result
          if (
            ts.isCallExpression(fn.parent) &&
            fn.parent.arguments.includes(fn as ts.Expression) &&
            ts.isPropertyAccessExpression(fn.parent.expression) &&
            ITERATORS.has(fn.parent.expression.name.text)
          ) {
            propagateValue(fn.parent, acc, visited, depth + 1);
          } else {
            const name = nameOfFunction(fn);
            if (name)
              mergeFlow(
                acc,
                flowFromReturn(fn, depth + 1),
                `return of ${name}`,
              );
          }
        }
        return;
      }
      if (ts.isSpreadElement(p)) {
        n = p;
        continue;
      }
      if (
        (ts.isCallExpression(p) || ts.isNewExpression(p)) &&
        p.arguments?.includes(n as ts.Expression)
      ) {
        // pass-through builtins: the call's result still carries the value
        const calleeText = p.expression.getText();
        if (
          /^(Object\.(entries|keys|values|assign|fromEntries)|Array\.from|JSON\.(stringify|parse)|String|Number|Boolean|structuredClone)$/.test(
            calleeText,
          )
        ) {
          n = p;
          continue;
        }
        // the call itself may be an effect site (console.log(...value...)): it carries the value
        const callSite = effectSites.find(
          (e) =>
            e.file === file &&
            e.pos === p.getStart(sf) &&
            e.endPos === p.getEnd(),
        );
        if (callSite) acc.sites.add(callSite.id);
        const index = p.arguments!.indexOf(n as ts.Expression);
        const target = projectCallee(p.expression);
        if (target) {
          const b = bindingFor(target, index, path);
          if (b) propagateBinding(b, acc, visited, depth + 1);
        } else {
          const c = unwrap(p.expression);
          const calleeName = ts.isIdentifier(c)
            ? c.text
            : ts.isPropertyAccessExpression(c)
              ? c.name.text
              : "";
          for (const b of externalSink(calleeName, path))
            acc.boundaries.push({ ...b, via: `${calleeName}(...)` });
          // items.push(value) on a local array: the value becomes part of the array
          if (
            ts.isPropertyAccessExpression(c) &&
            MUTATORS.has(c.name.text) &&
            ts.isCallExpression(p)
          ) {
            const root = rootOfExpr(c.expression);
            const d = ts.isIdentifier(root) ? declOf(root) : undefined;
            if (d && ts.isVariableDeclaration(d) && enclosingFunction(d)) {
              propagateBinding(d, acc, visited, depth + 1);
              return;
            }
          }
          // a library computation (`last(xs)`, `Session.fromPropertyArray(entries)`, `new Map(entries)`): its
          // result derives from its arguments, so the value flows on through the call. Not for calls a test
          // controls: sinks injected as parameters or fields, installed globals, mocked modules, I/O, timers, logs.
          if (carriesArguments(p, callSite)) {
            path = [];
            n = p;
            continue;
          }
        }
        return;
      }
      if (ts.isCallExpression(p) && p.expression === n) {
        // value is called: e.g. formatArgs(args) -> result flows onward
        n = p;
        continue;
      }
      if (
        ts.isCallExpression(p) &&
        ts.isPropertyAccessExpression(p.expression) &&
        p.expression.expression === n
      ) {
        // value is the receiver of a method call: iterators pass elements to callbacks; other methods yield derived values
        if (ITERATORS.has(p.expression.name.text)) {
          for (const arg of p.arguments)
            if (ts.isArrowFunction(arg) || ts.isFunctionExpression(arg))
              for (const prm of arg.parameters)
                propagateBinding(prm, acc, visited, depth + 1);
          if (p.expression.name.text !== "forEach") {
            n = p;
            continue;
          }
          return;
        }
        n = p;
        continue;
      }
      // `!value`, `typeof value`: still the value, seen through an operator
      if (
        (ts.isPrefixUnaryExpression(p) &&
          p.operator === ts.SyntaxKind.ExclamationToken) ||
        ts.isTypeOfExpression(p)
      ) {
        n = p;
        continue;
      }
      // the value is a condition: it selects which exit (return/throw) the enclosing function takes, so a test
      // that reads the function's result observes the value's truthiness (`if (!prompt) return null`,
      // `if (a && await blogExists(...)) return x`)
      if (
        (ts.isIfStatement(p) && p.expression === n) ||
        ((ts.isWhileStatement(p) || ts.isDoStatement(p)) &&
          p.expression === n) ||
        (ts.isForStatement(p) && p.condition === n) ||
        (ts.isSwitchStatement(p) && p.expression === n)
      ) {
        const exits = exitSitesSelectedBy(p, sf);
        for (const e of exits) acc.sites.add(e.id);
        if (exits.some((e) => e.category !== "throw")) {
          const fn = enclosingFunction(p);
          const name = nameOfFunction(fn);
          if (fn && name)
            mergeFlow(
              acc,
              flowFromReturn(fn, depth + 1),
              `selects the return of ${name}`,
            );
        }
        return;
      }
      if (
        ts.isExpressionStatement(p) ||
        ts.isBlock(p) ||
        ts.isIfStatement(p) ||
        ts.isArrowFunction(p) ||
        ts.isFunctionDeclaration(p) ||
        ts.isSourceFile(p)
      )
        return;
      n = p;
    }
  }
  /** The return/throw sites a condition chooses between: those in the branches it controls and, when the
   *  controlled branch leaves the block early, those in the rest of the block. */
  function exitSitesSelectedBy(ctrl: ts.Node, sf: ts.SourceFile): Site[] {
    const ranges: [number, number][] = [];
    if (ts.isIfStatement(ctrl)) {
      ranges.push([
        ctrl.thenStatement.getStart(sf),
        ctrl.thenStatement.getEnd(),
      ]);
      if (ctrl.elseStatement)
        ranges.push([
          ctrl.elseStatement.getStart(sf),
          ctrl.elseStatement.getEnd(),
        ]);
      else if (terminates(ctrl.thenStatement) && ts.isBlock(ctrl.parent))
        ranges.push([ctrl.getEnd(), ctrl.parent.getEnd()]);
    } else if (
      ts.isWhileStatement(ctrl) ||
      ts.isDoStatement(ctrl) ||
      ts.isForStatement(ctrl)
    ) {
      ranges.push([ctrl.statement.getStart(sf), ctrl.statement.getEnd()]);
      if (ts.isBlock(ctrl.parent))
        ranges.push([ctrl.getEnd(), ctrl.parent.getEnd()]);
    } else if (ts.isSwitchStatement(ctrl)) {
      ranges.push([ctrl.caseBlock.getStart(sf), ctrl.caseBlock.getEnd()]);
    }
    const file = rel(sf);
    return effectSites.filter(
      (e) =>
        e.file === file &&
        (e.category === "return" ||
          e.category === "throw" ||
          e.category === "callback-return") &&
        ranges.some(([a, b]) => e.pos >= a && e.endPos <= b),
    );
  }
  /** Does the result of this non-project call derive from its arguments (a library computation such as
   *  `last(xs)` or `Session.fromPropertyArray(entries)`) rather than from something the test controls (a sink
   *  injected as a parameter or field, an installed global, a mocked module, I/O, a timer, a log)? */
  function carriesArguments(
    call: ts.CallExpression | ts.NewExpression,
    site: Site | undefined,
  ): boolean {
    if (site && !(site.category === "external-call" && site.note === "import"))
      return false;
    const root = rootOfExpr(call.expression);
    if (!ts.isIdentifier(root)) return false;
    const d = declOf(root);
    // a parameter, or a global a test file installed: test-controlled
    if (d && (ts.isParameter(d) || ts.isBindingElement(d))) return false;
    if (!site) {
      if (d && !d.getSourceFile().isDeclarationFile) return true;
      for (const sinks of globalSinksByFile.values())
        if (sinks.has(root.text)) return false;
      return true;
    }
    const imp = importOf(root);
    return !!imp && !mockedAnywhere(imp);
  }
  /** Is this import replaced by `vi.mock` in some test file? Its results are then test-controlled. */
  function mockedAnywhere(imp: { spec: string; resolved?: string }): boolean {
    for (const mocks of moduleMocksByFile.values())
      for (const m of mocks)
        if (
          (m.resolved && imp.resolved && m.resolved === imp.resolved) ||
          m.spec === imp.spec
        )
          return true;
    return false;
  }
  function propagateBinding(
    decl: ts.Declaration,
    acc: FlowResult,
    visited: Set<string>,
    depth: number,
  ) {
    if (depth > 12) return;
    const sf = decl.getSourceFile();
    const key = `B:${rel(sf)}:${decl.getStart(sf)}`;
    if (visited.has(key)) return;
    visited.add(key);
    // parameter declared with a binding pattern: follow each element
    if (ts.isParameter(decl) && ts.isObjectBindingPattern(decl.name)) {
      for (const el of decl.name.elements)
        propagateBinding(el, acc, visited, depth);
      return;
    }
    const sym = symbolOf((decl as ts.NamedDeclaration).name ?? decl);
    if (!sym) return;
    const scope = enclosingFunction(decl) ?? sf;
    const visit = (n: ts.Node) => {
      if (
        ts.isIdentifier(n) &&
        n !== (decl as ts.NamedDeclaration).name &&
        symbolOf(n) === sym
      )
        propagateValue(n, acc, visited, depth + 1);
      ts.forEachChild(n, visit);
    };
    visit(scope);
  }
  function mergeFlow(acc: FlowResult, other: FlowResult, via: string) {
    for (const b of other.boundaries)
      acc.boundaries.push({ ...b, via: b.via ? `${via} → ${b.via}` : via });
    for (const s of other.sites) acc.sites.add(s);
  }
  /** Boundaries and sites reached by the return value of a project function. */
  function flowFromReturn(fn: ts.SignatureDeclaration, depth = 0): FlowResult {
    // depth cut-off must not poison the cache with an empty result
    if (depth > 8) return { boundaries: [], sites: new Set() };
    const key = fnKey(fn);
    const cached = flowCache.get(key);
    if (cached) return cached;
    const acc: FlowResult = { boundaries: [], sites: new Set() };
    flowCache.set(key, acc); // cycle guard
    const visited = new Set<string>();
    for (const sf of allFiles) {
      if (!isProdFile(sf)) continue;
      const visit = (n: ts.Node) => {
        if (
          (ts.isCallExpression(n) || ts.isNewExpression(n)) &&
          projectCallee(n.expression) === fn
        )
          propagateValue(n, acc, visited, depth + 1);
        // higher-order use: the function is passed as a value (e.g. `log({ formatArgs: debugFormatArgs })`)
        // and called through a parameter later; its return flows out of those calls
        else if (
          ts.isIdentifier(n) &&
          !(ts.isCallExpression(n.parent) && n.parent.expression === n) &&
          !ts.isPropertyAccessExpression(n.parent) &&
          functionOfIdentifier(n) === fn
        ) {
          for (const call of callsThroughParameter(n))
            propagateValue(call, acc, visited, depth + 1);
        }
        ts.forEachChild(n, visit);
      };
      visit(sf);
    }
    return acc;
  }
  function functionOfIdentifier(
    id: ts.Identifier,
  ): ts.SignatureDeclaration | undefined {
    const d = declOf(id);
    if (!d) return undefined;
    if (ts.isFunctionDeclaration(d)) return d;
    if (
      ts.isVariableDeclaration(d) &&
      d.initializer &&
      (ts.isArrowFunction(unwrap(d.initializer)) ||
        ts.isFunctionExpression(unwrap(d.initializer)))
    )
      return unwrap(d.initializer) as ts.SignatureDeclaration;
    return undefined;
  }
  /** For a function passed as an argument, the calls made through the receiving parameter. */
  function callsThroughParameter(valueRef: ts.Identifier): ts.CallExpression[] {
    let path: string[] = [];
    let n: ts.Node = valueRef;
    while (
      n.parent &&
      (ts.isPropertyAssignment(n.parent) ||
        ts.isShorthandPropertyAssignment(n.parent) ||
        ts.isObjectLiteralExpression(n.parent) ||
        ts.isParenthesizedExpression(n.parent))
    ) {
      if (
        ts.isPropertyAssignment(n.parent) ||
        ts.isShorthandPropertyAssignment(n.parent)
      )
        path = [n.parent.name.getText(), ...path];
      n = n.parent;
    }
    const p = n.parent;
    if (
      !p ||
      !(ts.isCallExpression(p) || ts.isNewExpression(p)) ||
      !p.arguments?.includes(n as ts.Expression)
    )
      return [];
    const target = projectCallee(p.expression);
    if (!target) return [];
    const binding = bindingFor(
      target,
      p.arguments!.indexOf(n as ts.Expression),
      path,
    );
    if (!binding) return [];
    const sym = symbolOf((binding as ts.NamedDeclaration).name ?? binding);
    if (!sym) return [];
    const calls: ts.CallExpression[] = [];
    const scope = ts.isClassDeclaration(target) ? target : target;
    const visit = (x: ts.Node) => {
      if (ts.isCallExpression(x)) {
        const c = unwrap(x.expression);
        if (ts.isIdentifier(c) && symbolOf(c) === sym) calls.push(x);
      }
      ts.forEachChild(x, visit);
    };
    visit(scope);
    return calls;
  }
  function functionByName(
    name: string,
    file: string,
  ): ts.SignatureDeclaration | undefined {
    const sf = srcByFile.get(file);
    if (!sf) return undefined;
    let found: ts.SignatureDeclaration | undefined;
    const visit = (n: ts.Node) => {
      if (found) return;
      if (ts.isFunctionLike(n) && nameOfFunction(n) === name) {
        found = n;
        return;
      }
      ts.forEachChild(n, visit);
    };
    visit(sf);
    return found;
  }

  // ---------------------------------------------------------------------------
  // Static tests: assertions, implicit oracles, sink bindings
  // ---------------------------------------------------------------------------
  /** alts: other roots the same expression can stand for (rows of an it.each table); thrown: the value came through a rejection handler. */
  interface Origin {
    kind: string;
    path: string[];
    facet?: string;
    obj?: ts.ObjectLiteralExpression;
    alts?: Origin[];
    thrown?: "only" | "also";
  }
  interface StaticTest {
    file: string;
    line: number;
    name: string;
    observations: Observation[];
    sinks: SinkBinding[];
    rendered: Set<string>;
    /** last line of the test declaration, and its title without quotes (for runners that report no line) */
    endLine?: number;
    title?: string;
    /** assertions whose operand the static analysis could not trace: the test statements that define the
     *  operand, for runtime statement attribution (supercov statement markers) */
    pending: PendingOperand[];
  }
  interface PendingOperand {
    assertionSource: string;
    assertionMethod: string;
    /** "file:line:column" of the statements that compute the operand (its own statement and the
     *  declarations/assignments of the variables it reads) */
    statements: string[];
    strength: Strength;
    negative: boolean;
    rejects: boolean;
    where: string;
    /** the operand's text and the origin kind that defeated the analysis, for the report's limit detail */
    shape: string;
  }

  // ---------------------------------------------------------------------------
  // Adapter: objects of mocks (`const mocks = vi.hoisted(() => ({ a: vi.fn(), b: { c: vi.fn() } }))`)
  // A path into such an object that ends at a mock is a sink: `sink:mocks.b.c`.
  // ---------------------------------------------------------------------------
  function returnedObject(fn: ts.Node): ts.ObjectLiteralExpression | undefined {
    if (
      !(
        ts.isArrowFunction(fn) ||
        ts.isFunctionExpression(fn) ||
        ts.isMethodDeclaration(fn) ||
        ts.isFunctionDeclaration(fn)
      )
    )
      return undefined;
    const body = fn.body;
    if (!body) return undefined;
    if (!ts.isBlock(body)) {
      const b = unwrap(body as ts.Expression);
      return ts.isObjectLiteralExpression(b) ? b : undefined;
    }
    let r: ts.ObjectLiteralExpression | undefined;
    const visit = (n: ts.Node) => {
      if (r) return;
      if (
        ts.isReturnStatement(n) &&
        n.expression &&
        ts.isObjectLiteralExpression(unwrap(n.expression))
      )
        r = unwrap(n.expression) as ts.ObjectLiteralExpression;
      ts.forEachChild(n, visit);
    };
    visit(body);
    return r;
  }
  /** The object literal behind `const x = {...}`, `const x = vi.hoisted(() => ({...}))` or `const x = makeMocks()` (a local factory). */
  function hoistedObject(
    init: ts.Expression,
  ): ts.ObjectLiteralExpression | undefined {
    const u = unwrap(init);
    if (ts.isObjectLiteralExpression(u)) return u;
    if (
      ts.isCallExpression(u) &&
      /^(vi|jest)\.hoisted$/.test(u.expression.getText()) &&
      u.arguments[0]
    )
      return returnedObject(unwrap(u.arguments[0]));
    if (ts.isCallExpression(u) && ts.isIdentifier(u.expression)) {
      const fn = localFunctionNode(u.expression.text, u.getSourceFile());
      if (fn) return returnedObject(fn);
    }
    return undefined;
  }
  /** The value a variable holds: its initializer, or for `let x` the first `x = ...` assignment in the file (beforeEach setup). */
  function initOf(d: ts.VariableDeclaration): ts.Expression | undefined {
    if (d.initializer) return d.initializer;
    if (!ts.isIdentifier(d.name)) return undefined;
    const sym = symbolOf(d.name);
    let found: ts.Expression | undefined;
    const visit = (n: ts.Node) => {
      if (found) return;
      if (
        ts.isBinaryExpression(n) &&
        n.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        ts.isIdentifier(n.left) &&
        symbolOf(n.left) === sym
      ) {
        found = n.right;
        return;
      }
      ts.forEachChild(n, visit);
    };
    visit(d.getSourceFile());
    return found;
  }
  const GLOBAL_ROOTS = new Set(["globalThis", "window", "global", "self"]);
  /** An object of mocks or a single mock, following identifiers to their (possibly later-assigned) value. */
  function mockValue(v: ts.Expression, depth = 0): ts.Expression | undefined {
    if (depth > 4) return undefined;
    const u = unwrap(v);
    if (isMockFactory(u)) return u;
    const obj = hoistedObject(u);
    if (obj) return containsMock(obj) ? obj : undefined;
    if (ts.isIdentifier(u)) {
      const d = declOf(u);
      const init = d && ts.isVariableDeclaration(d) ? initOf(d) : undefined;
      return init ? mockValue(init, depth + 1) : undefined;
    }
    return undefined;
  }
  function containsMock(obj: ts.Node): boolean {
    let found = false;
    const visit = (n: ts.Node) => {
      if (found) return;
      if (ts.isCallExpression(n) && isMockFactory(n)) found = true;
      else ts.forEachChild(n, visit);
    };
    visit(obj);
    return found;
  }
  /** Walk `path` through an object literal; the segments up to (and including) the first mock leaf, or undefined. */
  function mockLeafPath(
    obj: ts.ObjectLiteralExpression,
    path: string[],
  ): string[] | undefined {
    let cur: ts.Expression = obj;
    const walked: string[] = [];
    // a property of an object literal, or of any object-literal argument of Object.assign(target, ...sources)
    const propertyOf = (
      obj: ts.Expression,
      seg: string,
    ): ts.Expression | undefined => {
      if (
        ts.isCallExpression(obj) &&
        obj.expression.getText() === "Object.assign"
      ) {
        for (const a of [...obj.arguments].reverse()) {
          const found = propertyOf(unwrap(a), seg);
          if (found) return found;
        }
        return undefined;
      }
      if (!ts.isObjectLiteralExpression(obj)) return undefined;
      const p = obj.properties.find((x) => x.name?.getText() === seg);
      return p && ts.isPropertyAssignment(p)
        ? unwrap(p.initializer)
        : p && ts.isShorthandPropertyAssignment(p)
          ? p.name
          : undefined;
    };
    for (const seg of path) {
      let v = propertyOf(cur, seg);
      if (!v) return undefined;
      // `{ modalShow }` where `const modalShow = vi.fn()` was declared just above
      if (ts.isIdentifier(v)) {
        const d = declOf(v);
        const init = d && ts.isVariableDeclaration(d) ? initOf(d) : undefined;
        if (init) v = unwrap(init);
      }
      walked.push(seg);
      if (isMockFactory(v)) return walked;
      // `Article: articleResource()`: a nested local factory
      if (ts.isCallExpression(v)) {
        const nested = hoistedObject(v);
        if (nested) v = nested;
      }
      cur = v;
    }
    return undefined;
  }
  /** `mocks.fetcher.submit` → the sink object, its name and the path inside it. */
  function sinkObjOrigin(
    e: ts.Expression,
  ):
    | { objName: string; obj: ts.ObjectLiteralExpression; path: string[] }
    | undefined {
    const root = rootOfExpr(e);
    if (!ts.isIdentifier(root)) return undefined;
    const d = declOf(root);
    const init = d && ts.isVariableDeclaration(d) ? initOf(d) : undefined;
    if (!init) return undefined;
    const obj = hoistedObject(init);
    if (!obj || !containsMock(obj)) return undefined;
    return { objName: root.text, obj, path: chainOf(e).slice(1) };
  }

  // ---------------------------------------------------------------------------
  // Adapter: module mocks (`vi.mock('~/x', () => ({ a: { b: mocks.c }, useY: () => mocks.y }))`).
  // Production calls through the mocked import write into the bound sinks.
  // ---------------------------------------------------------------------------
  interface ExportBinding {
    path: string[];
    sink?: string;
    returns?: {
      objName: string;
      obj: ts.ObjectLiteralExpression;
      path: string[];
    };
    /** `vi.mock('~/lib/prisma.server', () => ({ prisma }))` with `prisma` an object of mocks: every leaf under
     *  the export is a sink named by its path inside that object */
    sinkObj?: {
      objName: string;
      obj: ts.ObjectLiteralExpression;
      path: string[];
    };
  }
  interface ModuleMock {
    spec: string;
    resolved?: string;
    exports: ExportBinding[];
  }
  const moduleMocksByFile = new Map<string, ModuleMock[]>();
  /**
   * Globals a test file installs: `Object.assign(globalThis, { shopify: { toast: { show: vi.fn() } } })`,
   * `window.Beacon = vi.fn()`, `vi.stubGlobal('shopify', stub)`. Value: an object of mocks or a single mock.
   */
  /** value: the mock or object of mocks; name: what the test calls it (`window.Beacon = beacon` → `beacon`), so both sides agree. */
  interface GlobalSink {
    value: ts.Expression;
    name: string;
  }
  const globalSinksByFile = new Map<string, Map<string, GlobalSink>>();
  function collectGlobalSinks(sf: ts.SourceFile): Map<string, GlobalSink> {
    const out = new Map<string, GlobalSink>();
    const set = (name: string, v: ts.Expression) => {
      const mv = mockValue(v);
      if (!mv || out.has(name)) return;
      const u = unwrap(v);
      const viaVariable =
        ts.isIdentifier(u) &&
        (() => {
          const d = declOf(u);
          return !!d && ts.isVariableDeclaration(d);
        })();
      out.set(name, {
        value: mv,
        name: viaVariable ? (u as ts.Identifier).text : name,
      });
    };
    const visit = (n: ts.Node) => {
      if (
        ts.isCallExpression(n) &&
        /^Object\.assign$/.test(n.expression.getText()) &&
        n.arguments.length >= 2 &&
        GLOBAL_ROOTS.has(n.arguments[0].getText())
      ) {
        const obj = unwrap(n.arguments[1]);
        if (ts.isObjectLiteralExpression(obj))
          for (const p of obj.properties) {
            if (ts.isPropertyAssignment(p) && p.name)
              set(p.name.getText(), p.initializer);
            else if (ts.isShorthandPropertyAssignment(p))
              set(p.name.text, p.name);
          }
      }
      if (
        ts.isCallExpression(n) &&
        /^(vi|jest)\.stubGlobal$/.test(n.expression.getText()) &&
        n.arguments.length >= 2 &&
        ts.isStringLiteralLike(n.arguments[0])
      )
        set(n.arguments[0].text, n.arguments[1]);
      if (
        ts.isBinaryExpression(n) &&
        n.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        ts.isPropertyAccessExpression(n.left) &&
        GLOBAL_ROOTS.has(n.left.expression.getText())
      )
        set(n.left.name.text, n.right);
      ts.forEachChild(n, visit);
    };
    visit(sf);
    return out;
  }
  /** Origin of `name` (or `window.name`) when the test file installed it as a mock or an object of mocks. */
  function globalSinkOrigin(
    name: string,
    sf: ts.SourceFile,
  ): Origin | undefined {
    const g = globalSinksByFile.get(relative(root, sf.fileName))?.get(name);
    if (!g) return undefined;
    return ts.isObjectLiteralExpression(g.value)
      ? { kind: "sinkobj:" + g.name, path: [], obj: g.value }
      : { kind: "sink:" + g.name, path: [] };
  }
  /** Boundary a production call `shopify.toast.show(...)` / `window.Beacon(...)` writes into, for one test file's installed globals. */
  function globalSinkBoundary(
    name: string,
    rest: string[],
    testFile: string,
  ): Boundary | undefined {
    const g = globalSinksByFile.get(testFile)?.get(name);
    if (!g) return undefined;
    if (ts.isObjectLiteralExpression(g.value)) {
      const leaf = mockLeafPath(g.value, rest);
      return leaf
        ? {
            boundary: `sink:${g.name}.${leaf.join(".")}`,
            via: "test-installed global",
          }
        : undefined;
    }
    return rest.length === 0
      ? { boundary: "sink:" + g.name, via: "test-installed global" }
      : undefined;
  }
  function resolveSpec(spec: string, fromFile: string): string | undefined {
    return program.resolveModule(spec, fromFile);
  }
  function collectModuleMocks(sf: ts.SourceFile): ModuleMock[] {
    const mocks: ModuleMock[] = [];
    const visit = (n: ts.Node) => {
      if (
        ts.isCallExpression(n) &&
        /^(vi|jest)\.(mock|doMock)$/.test(n.expression.getText()) &&
        n.arguments[0] &&
        ts.isStringLiteralLike(n.arguments[0])
      ) {
        const spec = n.arguments[0].text;
        const factory = n.arguments[1] ? unwrap(n.arguments[1]) : undefined;
        const exportsObj = factory ? returnedObject(factory) : undefined;
        const exportsList: ExportBinding[] = [];
        const walk = (obj: ts.ObjectLiteralExpression, path: string[]) => {
          for (const p of obj.properties) {
            if (ts.isSpreadAssignment(p) || !p.name) continue;
            const full = [...path, p.name.getText()];
            const v: ts.Node | undefined = ts.isPropertyAssignment(p)
              ? unwrap(p.initializer)
              : ts.isShorthandPropertyAssignment(p)
                ? p.name
                : ts.isMethodDeclaration(p)
                  ? p
                  : undefined;
            if (!v) continue;
            if (ts.isObjectLiteralExpression(v)) {
              walk(v, full);
              continue;
            }
            if (ts.isIdentifier(v) || ts.isPropertyAccessExpression(v)) {
              const so = sinkObjOrigin(v);
              const leaf = so ? mockLeafPath(so.obj, so.path) : undefined;
              if (so && leaf) {
                exportsList.push({
                  path: full,
                  sink: `sink:${so.objName}.${leaf.join(".")}`,
                });
                continue;
              }
              // `{ prisma }` where `prisma` is a (hoisted) object of mocks: the export is the whole sink object
              if (so) {
                exportsList.push({ path: full, sinkObj: so });
                continue;
              }
            }
            // `$setBlocksType: vi.fn()` right in the factory: a sink named after the module and export path
            if (ts.isCallExpression(v) && isMockFactory(v)) {
              exportsList.push({
                path: full,
                sink: `sink:${spec}#${full.join(".")}`,
              });
              continue;
            }
            if (
              ts.isArrowFunction(v) ||
              ts.isFunctionExpression(v) ||
              ts.isMethodDeclaration(v)
            ) {
              let ret: ts.Expression | undefined;
              if (v.body && !ts.isBlock(v.body))
                ret = unwrap(v.body as ts.Expression);
              else if (v.body) {
                const vr = (x: ts.Node) => {
                  if (ret) return;
                  if (ts.isReturnStatement(x) && x.expression)
                    ret = unwrap(x.expression);
                  ts.forEachChild(x, vr);
                };
                vr(v.body);
              }
              const rso =
                ret &&
                (ts.isIdentifier(ret) || ts.isPropertyAccessExpression(ret))
                  ? sinkObjOrigin(ret)
                  : undefined;
              exportsList.push(
                rso ? { path: full, returns: rso } : { path: full },
              );
            }
          }
        };
        if (exportsObj) walk(exportsObj, []);
        mocks.push({
          spec,
          resolved: resolveSpec(spec, sf.fileName),
          exports: exportsList,
        });
      }
      ts.forEachChild(n, visit);
    };
    visit(sf);
    return mocks;
  }
  function importOf(
    id: ts.Identifier,
  ): { spec: string; resolved?: string; importedName: string } | undefined {
    const d = checker.getSymbolAtLocation(id)?.declarations?.[0];
    if (!d) return undefined;
    let spec: string | undefined;
    let importedName = id.text;
    if (ts.isImportSpecifier(d)) {
      importedName = (d.propertyName ?? d.name).text;
      spec = (d.parent.parent.parent as ts.ImportDeclaration).moduleSpecifier
        .getText()
        .slice(1, -1);
    } else if (ts.isImportClause(d)) {
      importedName = "default";
      spec = (d.parent as ts.ImportDeclaration).moduleSpecifier
        .getText()
        .slice(1, -1);
    } else if (ts.isNamespaceImport(d)) {
      importedName = "*";
      spec = (d.parent.parent as ts.ImportDeclaration).moduleSpecifier
        .getText()
        .slice(1, -1);
    }
    if (!spec) return undefined;
    return {
      spec,
      resolved: resolveSpec(spec, id.getSourceFile().fileName),
      importedName,
    };
  }
  /** The declared name behind an import alias (`import { action as bulkAction }` → action). */
  function declaredName(d: ts.Declaration, fallback: string): string {
    const n = (d as ts.NamedDeclaration).name;
    return n && ts.isIdentifier(n) ? n.text : fallback;
  }
  /** `import { x } from '~/m'` in a test file that `vi.mock`s '~/m' with `x: vi.fn()` (or `x: mocks.y`): the sink. */
  function mockedImportOrigin(id: ts.Identifier): Origin | undefined {
    const imp = importOf(id);
    if (!imp) return undefined;
    const mocks =
      moduleMocksByFile.get(relative(root, id.getSourceFile().fileName)) ?? [];
    for (const m of mocks) {
      if (
        !(
          (m.resolved && imp.resolved && m.resolved === imp.resolved) ||
          m.spec === imp.spec
        )
      )
        continue;
      for (const b of m.exports)
        if (b.sink && b.path.length === 1 && b.path[0] === imp.importedName)
          return { kind: b.sink, path: [] };
    }
    return undefined;
  }
  /** `await import('~/lib/x')` / `vi.importActual('~/lib/x')`: the production module itself (kind carries the file). */
  function moduleOrigin(spec: string, from: ts.SourceFile): Origin | undefined {
    const resolved = resolveSpec(spec, from.fileName);
    const relPath = resolved ? relative(root, resolved) : "";
    return {
      kind: "module:" + (relPath.startsWith(srcDir + "/") ? relPath : ""),
      path: [],
    };
  }
  const mmCache = new Map<string, Boundary[]>();
  /** Sinks a production call site writes into, given the module mocks of one test file. */
  function moduleMockBoundaries(s: Site, testFile: string): Boundary[] {
    const key = `${s.id}|${testFile}`;
    const cached = mmCache.get(key);
    if (cached) return cached;
    const out: Boundary[] = [];
    const mocks = moduleMocksByFile.get(testFile) ?? [];
    const node = siteNodes.get(s.id);
    if (mocks.length && node && ts.isCallExpression(node)) {
      const callee = unwrap(node.expression);
      const chain = chainOf(callee);
      const root = rootOfExpr(callee);
      const same = (m: ModuleMock, imp: { spec: string; resolved?: string }) =>
        (m.resolved && imp.resolved && m.resolved === imp.resolved) ||
        m.spec === imp.spec;
      const exportPath = (imp: { importedName: string }, rest: string[]) =>
        imp.importedName === "default" || imp.importedName === "*"
          ? rest
          : [imp.importedName, ...rest];
      if (ts.isIdentifier(root)) {
        // shopify.toast.show(...) / window.Beacon(...) on a global the test installed (the app only has an ambient declaration for it)
        const rd = declOf(root);
        if (!rd || rd.getSourceFile().isDeclarationFile) {
          const gb =
            GLOBAL_ROOTS.has(root.text) && chain[1]
              ? globalSinkBoundary(chain[1], chain.slice(2), testFile)
              : globalSinkBoundary(root.text, chain.slice(1), testFile);
          if (gb) out.push(gb);
        }
        const imp = importOf(root);
        if (imp) {
          // authenticate.admin(request): the import itself is mocked
          const cp = exportPath(imp, chain.slice(1));
          for (const m of mocks)
            if (same(m, imp))
              for (const b of m.exports) {
                if (
                  b.sink &&
                  b.path.length === cp.length &&
                  b.path.every((x, i) => x === cp[i])
                )
                  out.push({ boundary: b.sink, via: `vi.mock('${m.spec}')` });
                // prisma.article.update(...) through `vi.mock('~/lib/prisma.server', () => ({ prisma }))`:
                // walk the rest of the call path inside the exported object of mocks
                if (
                  b.sinkObj &&
                  b.path.length < cp.length &&
                  b.path.every((x, i) => x === cp[i])
                ) {
                  const leaf = mockLeafPath(b.sinkObj.obj, [
                    ...b.sinkObj.path,
                    ...cp.slice(b.path.length),
                  ]);
                  if (leaf)
                    out.push({
                      boundary: `sink:${b.sinkObj.objName}.${leaf.join(".")}`,
                      via: `vi.mock('${m.spec}') ${b.path.join(".")}`,
                    });
                }
              }
        } else {
          // const fetcher = useFetcher(); fetcher.submit(...): the import returns an object of mocks
          const d = declOf(root);
          const decl =
            d && ts.isVariableDeclaration(d)
              ? d
              : d &&
                  ts.isBindingElement(d) &&
                  ts.isVariableDeclaration(d.parent.parent)
                ? d.parent.parent
                : undefined;
          const prefix =
            d && ts.isBindingElement(d)
              ? [((d.propertyName ?? d.name) as ts.Node).getText()]
              : [];
          const init = decl?.initializer ? unwrap(decl.initializer) : undefined;
          if (init && ts.isCallExpression(init)) {
            const ic = unwrap(init.expression);
            const ir = rootOfExpr(ic);
            const imp2 = ts.isIdentifier(ir) ? importOf(ir) : undefined;
            if (imp2) {
              const callPath = exportPath(imp2, chainOf(ic).slice(1));
              for (const m of mocks)
                if (same(m, imp2))
                  for (const b of m.exports) {
                    if (
                      !b.returns ||
                      b.path.length !== callPath.length ||
                      !b.path.every((x, i) => x === callPath[i])
                    )
                      continue;
                    const leaf = mockLeafPath(b.returns.obj, [
                      ...b.returns.path,
                      ...prefix,
                      ...chain.slice(1),
                    ]);
                    if (leaf)
                      out.push({
                        boundary: `sink:${b.returns.objName}.${leaf.join(".")}`,
                        via: `vi.mock('${m.spec}') ${b.path.join(".")}()`,
                      });
                  }
            }
          }
        }
      }
    }
    mmCache.set(key, out);
    return out;
  }
  const EXPECT_STRENGTH: Record<string, Strength> = {
    toEqual: "total",
    toStrictEqual: "total",
    toMatchObject: "value",
    toMatchSnapshot: "total",
    toMatchInlineSnapshot: "total",
    toHaveBeenCalledWith: "total",
    toHaveBeenLastCalledWith: "total",
    toHaveBeenNthCalledWith: "total",
    toBe: "value",
    toBeCloseTo: "value",
    toHaveLength: "value",
    toHaveProperty: "value",
    toBeInstanceOf: "value",
    toHaveBeenCalledTimes: "value",
    toHaveBeenCalledOnce: "value",
    toBeGreaterThan: "value",
    toBeGreaterThanOrEqual: "value",
    toBeLessThan: "value",
    toBeLessThanOrEqual: "value",
    toContain: "value",
    toContainEqual: "value",
    toMatch: "value",
    toThrow: "value",
    toThrowError: "value",
    toHaveTextContent: "value",
    toHaveAttribute: "value",
    toHaveValue: "value",
    toBeTruthy: "presence",
    toBeFalsy: "presence",
    toBeDefined: "presence",
    // toBeNull / toBeUndefined compare with one specific value: a `return null` mutated away fails them
    toBeUndefined: "value",
    toBeNull: "value",
    toBeNaN: "presence",
    toHaveBeenCalled: "presence",
    toBeInTheDocument: "presence",
    toBeVisible: "presence",
    toBeDisabled: "presence",
    toBeEnabled: "presence",
    toBeChecked: "presence",
    toHaveFocus: "presence",
    toBeEmptyDOMElement: "presence",
    toHaveClass: "value",
    toHaveStyle: "value",
    toHaveDisplayValue: "value",
    toHaveAccessibleName: "value",
    toEqualTypeOf: "presence",
    // Playwright locator and page matchers
    toHaveCount: "value",
    toHaveText: "value",
    toContainText: "value",
    toHaveURL: "value",
    toHaveTitle: "value",
    toHaveId: "value",
    toHaveCSS: "value",
    toHaveJSProperty: "value",
    toHaveValues: "value",
    toBeHidden: "presence",
    toBeAttached: "presence",
    toBeEditable: "presence",
    toBeFocused: "presence",
    toBeInViewport: "presence",
    toBeOK: "presence",
    toHaveScreenshot: "presence",
  };
  /** vi.fn(), jest.fn(), mock.fn(), vi.spyOn(...): a test-owned function sink, possibly with a chained mock setup. */
  function isMockFactory(e: ts.Expression): boolean {
    let cur: ts.Expression = unwrap(e);
    for (let i = 0; i < 12; i++) {
      if (ts.isCallExpression(cur)) {
        const c = unwrap(cur.expression);
        if (
          ts.isPropertyAccessExpression(c) &&
          /^(vi|jest|mock|sinon)$/.test(c.expression.getText()) &&
          /^(fn|spyOn|stub|spy|mock)$/.test(c.name.text)
        )
          return true;
        cur = c;
      } else if (ts.isPropertyAccessExpression(cur))
        cur = unwrap(cur.expression);
      else return false;
    }
    return false;
  }
  const ASSERT_STRENGTH: Record<string, Strength> = {
    deepEqual: "total",
    deepStrictEqual: "total",
    notDeepEqual: "total",
    notDeepStrictEqual: "total",
    equal: "value",
    strictEqual: "value",
    notEqual: "value",
    notStrictEqual: "value",
    match: "value",
    doesNotMatch: "value",
    ok: "presence",
    assert: "presence",
    rejects: "presence",
    throws: "presence",
    doesNotThrow: "presence",
    doesNotReject: "presence",
  };
  const staticTests: StaticTest[] = [];
  const pragmaCollector = collectPragmas(
    ts,
    allFiles.filter(isTestFile),
    rel,
    sites,
  );
  const staticTestKey = (file: string, line: number, name: string) =>
    JSON.stringify([file, line, name]);

  // Unlike origin tracing, comparison identity must NOT erase await, calls,
  // getters or transformations. Only syntax with no runtime operation is peeled.
  function comparisonExpression(e: ts.Expression): ts.Expression {
    while (
      ts.isParenthesizedExpression(e) ||
      ts.isNonNullExpression(e) ||
      ts.isAsExpression(e) ||
      ts.isTypeAssertionExpression(e) ||
      ts.isSatisfiesExpression(e)
    )
      e = e.expression;
    return e;
  }

  function comparisonLocation(n: ts.Node): string {
    const sf = n.getSourceFile();
    return `${rel(sf)}:${n.getStart(sf)}:${n.getEnd()}`;
  }

  function immutableBinding(
    expr: ts.Expression,
    seen = new Set<ts.Node>(),
  ): string | undefined {
    const e = comparisonExpression(expr);
    if (!ts.isIdentifier(e) || seen.size >= 32) return undefined;
    const d = declOf(e);
    if (
      !d ||
      !ts.isVariableDeclaration(d) ||
      !ts.isIdentifier(d.name) ||
      !d.initializer ||
      !ts.isVariableDeclarationList(d.parent) ||
      !(d.parent.flags & ts.NodeFlags.Const) ||
      seen.has(d)
    )
      return undefined;
    seen.add(d);
    // A copy of a mutable binding has its own identity, not a live alias.
    return immutableBinding(d.initializer, seen) ?? comparisonLocation(d);
  }

  function nativeComparison(call: ts.CallExpression): Comparison | undefined {
    const callee = comparisonExpression(call.expression);
    if (!ts.isPropertyAccessExpression(callee) || call.arguments.length < 2)
      return undefined;
    const receiver = comparisonExpression(callee.expression);
    if (!ts.isIdentifier(receiver)) return undefined;
    // Read the import declaration itself, before resolving its module alias.
    // A helper named `assert` is not the native assertion implementation.
    const declaration =
      checker.getSymbolAtLocation(receiver)?.declarations?.[0];
    if (
      !declaration ||
      !(ts.isImportClause(declaration) || ts.isNamespaceImport(declaration))
    )
      return undefined;
    const imported = ts.isImportClause(declaration)
      ? declaration.parent
      : declaration.parent.parent;
    if (!ts.isStringLiteralLike(imported.moduleSpecifier)) return undefined;
    const module = imported.moduleSpecifier.text;
    if (
      ![
        "node:assert",
        "node:assert/strict",
        "assert",
        "assert/strict",
      ].includes(module)
    )
      return undefined;
    const strict = module.endsWith("/strict");
    const predicates: Record<string, string> = {
      equal: strict ? "node-same-value" : "node-loose-equality",
      strictEqual: "node-same-value",
      deepEqual: strict ? "node-deep-strict-equality" : "node-deep-equality",
      deepStrictEqual: "node-deep-strict-equality",
    };
    const predicate = predicates[callee.name.text];
    if (!predicate) return undefined;
    const operand = (arg: ts.Expression): ComparisonOperand => ({
      source: comparisonLocation(arg),
      binding: immutableBinding(arg),
    });
    const actual = operand(call.arguments[0]);
    const expected = operand(call.arguments[1]);
    return {
      predicate,
      actual,
      expected,
      relation:
        actual.binding && actual.binding === expected.binding
          ? "same-immutable-binding"
          : "unresolved",
    };
  }

  function nativeContextMock(call: ts.CallExpression): boolean {
    const callee = unwrap(call.expression);
    if (!ts.isPropertyAccessExpression(callee)) return false;
    const tracker = unwrap(callee.expression);
    if (!ts.isPropertyAccessExpression(tracker) || tracker.name.text !== "mock")
      return false;
    const context = declOf(unwrap(tracker.expression));
    if (!context || !ts.isParameter(context)) return false;
    const callback = context.parent;
    if (
      !(ts.isArrowFunction(callback) || ts.isFunctionExpression(callback)) ||
      callback.parameters[0] !== context ||
      !ts.isCallExpression(callback.parent)
    )
      return false;
    const registration = unwrap(callback.parent.expression);
    if (!ts.isIdentifier(registration)) return false;
    const declaration =
      checker.getSymbolAtLocation(registration)?.declarations?.[0];
    if (
      !declaration ||
      !(ts.isImportSpecifier(declaration) || ts.isImportClause(declaration))
    )
      return false;
    if (
      ts.isImportSpecifier(declaration) &&
      !["test", "it"].includes((declaration.propertyName ?? declaration.name).text)
    )
      return false;
    const imported = ts.isImportSpecifier(declaration)
      ? declaration.parent.parent.parent
      : declaration.parent;
    return (
      ts.isStringLiteralLike(imported.moduleSpecifier) &&
      imported.moduleSpecifier.text === "node:test"
    );
  }

  function mockProjection(o: Origin): MockProjection | undefined {
    if (!o.kind.startsWith("mock:console.")) return undefined;
    const path = o.path;
    const joined = path.join(".");
    const kind =
      joined === "mock.callCount()" || joined === "mock.calls.length"
        ? "call-count"
        : joined === "mock.calls"
          ? "call-history"
          : /(?:^|\.)arguments(?:\.\[\d+\])?$/.test(joined)
            ? "call-arguments"
            : "projection";
    return { target: o.kind.slice(5), kind, path };
  }

  function originOf(expr: ts.Expression, depth = 0): Origin | undefined {
    // each property-access segment costs one level: `admin.rest.resources.Article.find.mock.calls.map(...)` read
    // through a local helper is 10 deep; runaway recursion through helpers is bounded by paramBindings instead
    if (depth > 16) return undefined;
    const e = unwrap(expr);
    if (
      ts.isStringLiteralLike(e) ||
      ts.isNumericLiteral(e) ||
      e.kind === ts.SyntaxKind.TrueKeyword ||
      e.kind === ts.SyntaxKind.FalseKeyword ||
      e.kind === ts.SyntaxKind.NullKeyword ||
      ts.isRegularExpressionLiteral(e) ||
      ts.isTemplateExpression(e) ||
      ts.isArrayLiteralExpression(e) ||
      ts.isObjectLiteralExpression(e)
    )
      return { kind: "literal", path: [] };
    if (ts.isNewExpression(e))
      return {
        kind: /Client|Transport|WebSocket/.test(e.expression.getText())
          ? "sdk-client"
          : "new:" + e.expression.getText(),
        path: [],
      };
    if (ts.isCallExpression(e)) {
      const callee = unwrap(e.expression);
      if (
        ts.isPropertyAccessExpression(callee) &&
        callee.name.text === "method" &&
        callee.expression.getText().endsWith(".mock") &&
        e.arguments.length >= 2 &&
        ts.isStringLiteralLike(e.arguments[1])
      ) {
        // Keep a same-named user helper out of the native mock model. Unsupported
        // tracker/receiver aliases remain unknown rather than acquiring stream credit.
        const receiver = unwrap(e.arguments[0]);
        const declaration = declOf(receiver);
        if (
          !nativeContextMock(e) ||
          !ts.isIdentifier(receiver) ||
          receiver.text !== "console" ||
          (declaration && !declaration.getSourceFile().isDeclarationFile)
        )
          return undefined;
        return {
          kind: `mock:${e.arguments[0].getText()}.${e.arguments[1].text}`,
          path: [],
        };
      }
      if (ts.isIdentifier(callee)) {
        // Resolve the declaration below. A familiar helper name is not a
        // contract, nor does it identify the particular process/socket observed.
        // testing-library: render(...)/within(...) results read the DOM; renderHook(() => useX()) reads useX's return
        const rawCallee =
          checker.getSymbolAtLocation(callee)?.declarations?.[0];
        const importedFromTestingLibrary =
          !!rawCallee &&
          (ts.isImportSpecifier(rawCallee) || ts.isImportClause(rawCallee)) &&
          /^@testing-library\//.test(
            (ts.isImportSpecifier(rawCallee)
              ? rawCallee.parent.parent.parent
              : rawCallee.parent
            ).moduleSpecifier
              .getText()
              .slice(1, -1),
          );
        if (
          (callee.text === "render" || callee.text === "within") &&
          importedFromTestingLibrary
        )
          return { kind: "dom", path: [] };
        if (callee.text === "renderHook" && e.arguments[0]) {
          let hook: Origin | undefined;
          const visit = (n: ts.Node) => {
            if (hook) return;
            if (
              ts.isCallExpression(n) &&
              ts.isIdentifier(unwrap(n.expression))
            ) {
              const d = declOf(unwrap(n.expression));
              if (d && isProdFile(d.getSourceFile()))
                hook = {
                  kind: "prod:" + (unwrap(n.expression) as ts.Identifier).text,
                  path: [],
                };
            }
            ts.forEachChild(n, visit);
          };
          visit(e.arguments[0]);
          if (hook) return hook;
        }
        if (["String", "Number", "Boolean"].includes(callee.text)) {
          const base = e.arguments[0]
            ? originOf(e.arguments[0], depth + 1)
            : undefined;
          return base?.kind.startsWith("mock:")
            ? { ...base, path: [...base.path, `${callee.text}()`] }
            : base;
        }
        const d = declOf(callee);
        if (d && isProdFile(d.getSourceFile()))
          return { kind: "prod:" + declaredName(d, callee.text), path: [] };
        if (d && isExternalDecl(d)) {
          // an import the test file mocked: calling it returns nothing observable, but the mock itself is a sink
          const mocked = mockedImportOrigin(callee);
          return mocked ?? { kind: "import:" + callee.text, path: [] };
        }
        // withEditor(run => ...) { return run(editor) }: the parameter is bound to the caller's callback
        if (d && ts.isParameter(d)) {
          const bound = boundArgument(d);
          const bf = bound ? unwrap(bound) : undefined;
          if (bf && (ts.isArrowFunction(bf) || ts.isFunctionExpression(bf))) {
            const viaCb = localFnCallOrigin(e, bf, depth + 1);
            if (viaCb) return viaCb;
          }
        }
        // const generate = await load(); generate(...): a variable holding a production function (or an it.each cell)
        if (
          d &&
          (ts.isVariableDeclaration(d) ||
            ts.isBindingElement(d) ||
            ts.isParameter(d))
        ) {
          const held = originOf(callee, depth + 1);
          if (held && held.kind.startsWith("prod:") && !held.path.length)
            return held;
          // const { getImage } = renderX(); getImage(): a local closure, follow its return expression
          const fn = heldFunction(d);
          if (fn) {
            const viaHeld = localFnCallOrigin(e, fn, depth + 1);
            if (viaHeld) return viaHeld;
          }
        }
        // a local helper that returns a wrapped value: follow its return expression, substituting
        // parameters with the call's arguments (`readLoaderJson(response)` → origin of `response`)
        const local = localFunctionNode(callee.text, e.getSourceFile());
        if (local) {
          const viaReturn = localFnCallOrigin(e, local, depth + 1);
          if (viaReturn) return viaReturn;
        }
        return { kind: "localfn:" + callee.text, path: [] };
      }
      if (ts.isPropertyAccessExpression(callee)) {
        const name = callee.name.text;
        const objText = callee.expression.getText();
        if (["JSON", "Object", "Array", "Promise"].includes(objText)) {
          const base = e.arguments[0]
            ? originOf(e.arguments[0], depth + 1)
            : undefined;
          return base?.kind.startsWith("mock:")
            ? { ...base, path: [...base.path, `${objText}.${name}()`] }
            : base;
        }
        // vi.mocked(x) is x; vi.importActual('~/x') is the real module
        if (
          (objText === "vi" || objText === "jest") &&
          name === "mocked" &&
          e.arguments[0]
        )
          return originOf(e.arguments[0], depth + 1);
        if (
          (objText === "vi" || objText === "jest") &&
          (name === "importActual" || name === "requireActual") &&
          e.arguments[0] &&
          ts.isStringLiteralLike(e.arguments[0])
        )
          return moduleOrigin(e.arguments[0].text, e.getSourceFile());
        const base = originOf(callee.expression, depth + 1);
        if (!base) return undefined;
        const step = base.kind.startsWith("mock:")
          ? `${name}(${e.arguments.map((arg) => arg.getText()).join(", ")})`
          : name + "()";
        const o: Origin = { ...base, path: [...base.path, step] };
        // promise.catch(e => e) carries the rejection; promise.then(onOk, onErr) carries either
        if (name === "catch" && e.arguments[0]) o.thrown = "only";
        else if (name === "then" && e.arguments.length >= 2) o.thrown = "also";
        if (
          name === "get" &&
          e.arguments[0] &&
          ts.isStringLiteralLike(e.arguments[0])
        )
          o.facet = e.arguments[0].text.toLowerCase();
        if (
          name === "includes" &&
          e.arguments[0] &&
          ts.isStringLiteralLike(e.arguments[0])
        )
          o.facet = "includes:" + e.arguments[0].text;
        // `.filter(line => line.startsWith('X'))` / `.filter(line => /re/.test(line))`: a total assertion on the
        // filtered subset pins only the messages the predicate selects
        if (
          (name === "filter" || name === "find" || name === "some") &&
          e.arguments[0]
        ) {
          const src = e.arguments[0].getText();
          const lit = /(?:startsWith|includes|endsWith)\((['"`])(.*?)\1\)/.exec(
            src,
          );
          const re = /(\/(?:[^/\\]|\\.)+\/[a-z]*)\.test\(/.exec(src);
          if (lit) o.facet = "includes:" + lit[2];
          else if (re)
            o.facet = "pattern:" + re[1].replace(/^\/|\/[a-z]*$/g, "");
        }
        return o;
      }
      // await import('~/lib/x'): the real module
      if (
        e.expression.kind === ts.SyntaxKind.ImportKeyword &&
        e.arguments[0] &&
        ts.isStringLiteralLike(e.arguments[0])
      )
        return moduleOrigin(e.arguments[0].text, e.getSourceFile());
      return undefined;
    }
    if (ts.isPropertyAccessExpression(e)) {
      // window.Beacon / globalThis.prisma: a test-installed mock, else production global state read back
      if (ts.isIdentifier(e.expression) && GLOBAL_ROOTS.has(e.expression.text))
        return (
          globalSinkOrigin(e.name.text, e.getSourceFile()) ?? {
            kind: "global",
            path: [e.name.text],
          }
        );
      const b = originOf(e.expression, depth + 1);
      if (b?.kind.startsWith("localfn:") && !b.path.length)
        return (
          localHelperProperty(
            b.kind.slice(8),
            e.name.text,
            e.getSourceFile(),
            depth + 1,
          ) ?? { ...b, path: [e.name.text] }
        );
      // (await import('~/x')).fn: an export of a production module
      if (b?.kind.startsWith("module:") && !b.path.length)
        return b.kind.slice(7)
          ? { kind: "prod:" + e.name.text, path: [] }
          : undefined;
      return b ? { ...b, path: [...b.path, e.name.text] } : undefined;
    }
    if (ts.isElementAccessExpression(e)) {
      const b = originOf(e.expression, depth + 1);
      const step = b?.kind.startsWith("mock:")
        ? `[${e.argumentExpression.getText()}]`
        : "[]";
      return b ? { ...b, path: [...b.path, step] } : undefined;
    }
    if (ts.isConditionalExpression(e)) return undefined; // selected branch needs value-flow evidence
    if (ts.isBinaryExpression(e)) {
      // Comma and simple assignment evaluate both operands but return only the right.
      if (
        e.operatorToken.kind === ts.SyntaxKind.CommaToken ||
        e.operatorToken.kind === ts.SyntaxKind.EqualsToken
      )
        return originOf(e.right, depth + 1);
      // Other operators transform or select values; do not guess their origin
      // from the first operand whose name can be resolved.
      return undefined;
    }
    if (ts.isPrefixUnaryExpression(e)) {
      const base = originOf(e.operand, depth + 1);
      return base?.kind.startsWith("mock:")
        ? { ...base, path: [...base.path, `unary:${e.operator}`] }
        : base;
    }
    if (ts.isIdentifier(e)) {
      // imports first, by their import declaration: alias resolution can fail on deep re-exports
      const raw = checker.getSymbolAtLocation(e)?.declarations?.[0];
      if (
        raw &&
        (ts.isImportSpecifier(raw) ||
          ts.isImportClause(raw) ||
          ts.isNamespaceImport(raw))
      ) {
        const spec = (
          raw.getSourceFile() &&
          ((ts.isImportSpecifier(raw)
            ? raw.parent.parent.parent
            : ts.isImportClause(raw)
              ? raw.parent
              : raw.parent.parent) as ts.ImportDeclaration)
        ).moduleSpecifier
          .getText()
          .slice(1, -1);
        if (
          /^@testing-library\//.test(spec) &&
          ["screen", "within", "render"].includes(e.text)
        )
          return { kind: "dom", path: [] };
        // expect($setBlocksType).toHaveBeenCalledWith(...): an import the test file mocked is a sink
        const mocked = mockedImportOrigin(e);
        if (mocked) return mocked;
        const target = declOf(e);
        if (target && isProdFile(target.getSourceFile()))
          return { kind: "prod:" + declaredName(target, e.text), path: [] };
        if (!target || isExternalDecl(target))
          return { kind: "import:" + e.text, path: [] };
      }
      const d = declOf(e);
      // a parameter of a local helper whose call we are following: use the call's argument
      if (d && ts.isParameter(d)) {
        const bound = boundArgument(d);
        if (bound) return originOf(bound, depth + 1);
        const cell = eachTableOrigin(d, depth);
        if (cell) return cell;
      }
      if (!d || d.getSourceFile().isDeclarationFile) {
        // an undeclared (or ambient) identifier the test file installed as a global object of mocks
        const g = globalSinkOrigin(e.text, e.getSourceFile());
        if (g) return g;
        if (e.text === "console") return { kind: "console", path: [] };
        if (!d) return undefined;
      }
      if (ts.isVariableDeclaration(d)) {
        const initializer = initOf(d);
        if (!initializer) {
          const forOf = d.parent.parent;
          if (ts.isForOfStatement(forOf)) {
            const b = originOf(forOf.expression, depth + 1);
            return b ? { ...b, path: [...b.path, "[]"] } : undefined;
          }
          return undefined;
        }
        const init = unwrap(initializer);
        // an object of mocks (plain or vi.hoisted): paths into it are sinks
        const hoisted = hoistedObject(init);
        if (hoisted && containsMock(hoisted))
          return { kind: "sinkobj:" + e.text, path: [], obj: hoisted };
        if (
          ts.isArrayLiteralExpression(init) ||
          ts.isObjectLiteralExpression(init) ||
          isMockFactory(init)
        )
          return { kind: "sink:" + e.text, path: [] };
        // test-owned accumulator fed from a child process stream: let output = ''; proc.stdout.on('data', c => output += c)
        if (ts.isStringLiteralLike(init) && init.text === "") {
          const sym = symbolOf(d.name);
          const scope = enclosingFunction(d) ?? d.getSourceFile();
          let fed: string | undefined;
          const scan = (n: ts.Node) => {
            if (fed) return;
            if (
              ts.isBinaryExpression(n) &&
              n.operatorToken.kind === ts.SyntaxKind.PlusEqualsToken &&
              ts.isIdentifier(n.left) &&
              symbolOf(n.left) === sym
            ) {
              let p: ts.Node | undefined = n;
              while (p && !fed) {
                if (
                  ts.isCallExpression(p) &&
                  ts.isPropertyAccessExpression(p.expression) &&
                  (p.expression.name.text === "on" ||
                    p.expression.name.text === "once")
                ) {
                  const recv = p.expression.expression.getText();
                  fed = /stderr/.test(recv) ? "proc-stderr" : "proc-stdout";
                }
                p = p.parent;
              }
            }
            ts.forEachChild(n, scan);
          };
          scan(scope);
          if (fed) return { kind: fed, path: [] };
        }
        // promise resolved from a child process 'close'/'exit' event carries the exit code
        if (
          ts.isNewExpression(init) &&
          init.expression.getText() === "Promise" &&
          /\.(once|on)\(\s*['"](close|exit)['"]/.test(init.getText())
        )
          return { kind: "proc-exit", path: [] };
        // Mutable scalar aliases need reaching definitions, not the initializer
        // or first assignment anywhere in the file. Container/stream cases above
        // retain their separate models.
        if (!(d.parent.flags & ts.NodeFlags.Const)) return undefined;
        return originOf(initializer, depth + 1);
      }
      if (ts.isBindingElement(d)) {
        const pattern = d.parent;
        const decl = pattern.parent;
        // ({ action, url }) => ... of a describe.each table
        if (ts.isParameter(decl)) {
          const cell = eachTableOrigin(d, depth);
          if (cell) return cell;
        }
        if (ts.isVariableDeclaration(decl) && decl.initializer) {
          const b = originOf(decl.initializer, depth + 1);
          const prop = ts.isObjectBindingPattern(pattern)
            ? ((d.propertyName ?? d.name) as ts.Node).getText()
            : "[]";
          // destructured from a local test helper: follow the helper's returned object property
          if (b?.kind.startsWith("localfn:") && prop !== "[]")
            return (
              localHelperProperty(
                b.kind.slice(8),
                prop,
                d.getSourceFile(),
                depth + 1,
              ) ?? { ...b, path: [...b.path, prop] }
            );
          return b ? { ...b, path: [...b.path, prop] } : undefined;
        }
        return { kind: "literal", path: [] };
      }
      if (ts.isParameter(d)) {
        const fn = d.parent;
        if (
          (ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) &&
          ts.isCallExpression(fn.parent) &&
          ts.isPropertyAccessExpression(fn.parent.expression) &&
          ITERATORS.has(fn.parent.expression.name.text)
        ) {
          const b = originOf(fn.parent.expression.expression, depth + 1);
          return b ? { ...b, path: [...b.path, "[]"] } : undefined;
        }
        return undefined;
      }
      if (isExternalDecl(d))
        return e.text === "screen"
          ? { kind: "dom", path: [] }
          : { kind: "import:" + e.text, path: [] };
      if (isProdFile(d.getSourceFile()))
        return { kind: "prod:" + e.text, path: [] };
      return undefined;
    }
    return undefined;
  }
  function boundariesOf(o: Origin): Boundary[] {
    if (o.alts?.length) {
      // every row of an it.each table: the same path read off each alternative root
      const all = [
        ...boundariesOf({ ...o, alts: undefined }),
        ...o.alts.flatMap((a) =>
          boundariesOf({
            ...a,
            path: [...a.path, ...o.path],
            facet: o.facet ?? a.facet,
            thrown: o.thrown,
            alts: undefined,
          }),
        ),
      ];
      const seen = new Set<string>();
      return all.filter((b) => {
        const k = `${b.boundary}|${b.facet ?? ""}`;
        if (seen.has(k)) return false;
        seen.add(k);
        return true;
      });
    }
    const p = o.path;
    const has = (...names: string[]) => names.some((n) => p.includes(n));
    const facetPath = (from: number) =>
      p
        .slice(from)
        .filter((x) => !x.endsWith("()") && x !== "[]")
        .join(".");
    switch (o.kind) {
      case "helper:rpc":
        if (p[0] === "response") {
          if (has("status", "ok")) return [{ boundary: "client-status" }];
          if (has("headers"))
            return [{ boundary: "client-header", facet: o.facet ?? "*" }];
          return [{ boundary: "client-status" }];
        }
        if (p[0] === "messages")
          return [{ boundary: "client-message", facet: facetPath(1) }];
        return [{ boundary: "client-status" }, { boundary: "client-message" }];
      case "helper:fetch":
        if (has("status", "ok")) return [{ boundary: "client-status" }];
        if (has("headers"))
          return [{ boundary: "client-header", facet: o.facet ?? "*" }];
        if (has("text()", "json()")) return [{ boundary: "client-message" }];
        return [{ boundary: "client-status" }];
      case "helper:pendingRpc":
        if (has("status")) return [{ boundary: "client-status" }];
        if (has("text")) return [{ boundary: "client-message" }];
        return [{ boundary: "client-status" }];
      case "helper:launchGateway":
        if (has("errors()")) return [{ boundary: "stderr" }];
        if (has("output()")) return [{ boundary: "stdout" }];
        if (has("exited")) return [{ boundary: "exit" }];
        if (has("ready()", "waitFor()"))
          return [{ boundary: "stdout" }, { boundary: "stderr" }];
        return [];
      case "helper:stdioRpc":
        return [{ boundary: "client-message", facet: facetPath(0) }];
      case "helper:once":
        return [
          {
            boundary:
              o.facet === "message" ? "client-message" : "client-lifecycle",
          },
        ];
      case "helper:lifecycleControl":
        return has("started") ? [{ boundary: "child-stdin" }] : [];
      case "helper:wireUpstream":
        return [{ boundary: "upstream" }];
      case "helper:recordingPeer":
        return has("received()") ? [{ boundary: "child-stdin" }] : [];
      case "proc-stdout":
        return [{ boundary: "stdout" }];
      case "proc-stderr":
        return [{ boundary: "stderr" }];
      case "proc-exit":
        return [{ boundary: "exit" }];
      case "sdk-client":
        if (has("sessionId"))
          return [{ boundary: "client-header", facet: "mcp-session-id" }];
        return [{ boundary: "client-message", facet: facetPath(0) }];
      case "dom":
        return [{ boundary: "dom" }];
      case "console":
        // expect(console.error).toHaveBeenCalled…: a spied stream; the facet keeps console.dir from pinning console.log sites
        return p[0]
          ? [
              {
                boundary:
                  p[0] === "error" || p[0] === "warn" ? "stderr" : "stdout",
                facet: "console." + p[0],
              },
            ]
          : [];
      case "global":
        // expect(globalThis.prisma).toBe(...): production global state read back
        return p[0] ? [{ boundary: "global:" + p[0] }] : [];
      default:
        if (o.kind.startsWith("prod:")) {
          const fn = o.kind.slice(5);
          const ret: Boundary = {
            boundary: "return:" + fn,
            facet: facetPath(0),
          };
          if (o.thrown === "only") return [{ boundary: "throw:" + fn }];
          if (o.thrown === "also") return [{ boundary: "throw:" + fn }, ret];
          return [ret];
        }
        // new PrismaSessionStorage(...).storeSession(...): the method's return
        if (o.kind.startsWith("new:") && p[0]?.endsWith("()"))
          return [
            {
              boundary: `return:${o.kind.slice(4)}.${p[0].slice(0, -2)}`,
              facet: facetPath(1),
            },
          ];
        if (o.kind.startsWith("sink:")) return [{ boundary: o.kind }];
        if (o.kind.startsWith("sinkobj:") && o.obj) {
          // mocks.fetcher.submit.mock.calls → sink:mocks.fetcher.submit; mocks.fetcher.state → test input, not a sink
          const leaf = mockLeafPath(
            o.obj,
            p.filter((x) => !x.endsWith("()") && x !== "[]"),
          );
          return leaf
            ? [{ boundary: `sink:${o.kind.slice(8)}.${leaf.join(".")}` }]
            : [];
        }
        // t.mock.method(console, 'log'): a test-owned replacement of a stream sink
        if (o.kind.startsWith("mock:console."))
          return [
            {
              boundary: /\.(error|warn)$/.test(o.kind) ? "stderr" : "stdout",
              facet: o.kind.slice(5),
            },
          ];
        return [];
    }
  }

  /** A local test helper `function name() { ...; return { prop: value } }`: the origin of `value`. */
  const localFnCache = new Map<string, ts.SignatureDeclaration | null>();
  function localFunctionNode(
    name: string,
    sf: ts.SourceFile,
  ): ts.SignatureDeclaration | undefined {
    const key = `${sf.fileName}|${name}`;
    const cached = localFnCache.get(key);
    if (cached !== undefined) return cached ?? undefined;
    let found: ts.SignatureDeclaration | undefined;
    const visit = (n: ts.Node) => {
      if (found) return;
      if (ts.isFunctionDeclaration(n) && n.name?.text === name) found = n;
      else if (
        ts.isVariableDeclaration(n) &&
        ts.isIdentifier(n.name) &&
        n.name.text === name &&
        n.initializer &&
        (ts.isArrowFunction(unwrap(n.initializer)) ||
          ts.isFunctionExpression(unwrap(n.initializer)))
      )
        found = unwrap(n.initializer) as ts.SignatureDeclaration;
      ts.forEachChild(n, visit);
    };
    visit(sf);
    localFnCache.set(key, found ?? null);
    return found;
  }
  /** The expression a local helper returns under property `prop` of its returned object literal. */
  function localHelperPropertyNode(
    name: string,
    prop: string,
    sf: ts.SourceFile,
  ): ts.Expression | undefined {
    const fn = localFunctionNode(name, sf);
    if (!fn) return undefined;
    let result: ts.Expression | undefined;
    const fromObject = (obj: ts.ObjectLiteralExpression) => {
      const p = obj.properties.find((x) => x.name?.getText() === prop);
      if (p && ts.isPropertyAssignment(p)) result = p.initializer;
      else if (p && ts.isShorthandPropertyAssignment(p)) result = p.name;
    };
    if (
      ts.isArrowFunction(fn) &&
      !ts.isBlock(fn.body) &&
      ts.isObjectLiteralExpression(unwrap(fn.body))
    )
      fromObject(unwrap(fn.body) as ts.ObjectLiteralExpression);
    const visit = (n: ts.Node) => {
      if (result) return;
      if (
        ts.isReturnStatement(n) &&
        n.expression &&
        ts.isObjectLiteralExpression(unwrap(n.expression))
      )
        fromObject(unwrap(n.expression) as ts.ObjectLiteralExpression);
      ts.forEachChild(n, visit);
    };
    visit(fn);
    return result;
  }
  /** While following a local helper call, its parameters stand for the call's arguments. */
  const paramBindings: Map<ts.Declaration, ts.Expression>[] = [];
  function boundArgument(d: ts.Declaration): ts.Expression | undefined {
    for (let i = paramBindings.length - 1; i >= 0; i--) {
      const bound = paramBindings[i].get(d);
      if (bound) return bound;
    }
    return undefined;
  }
  /**
   * `describe.each(rows)('%s', (a, b) => ...)` / `it.each(rows)('%s', ({ action }) => ...)`: a callback parameter
   * stands for one cell per row. Returns the row cells the parameter (or destructured property) can hold.
   */
  function eachTableCells(d: ts.Declaration): ts.Expression[] | undefined {
    let param: ts.ParameterDeclaration | undefined;
    let prop: string | undefined;
    if (ts.isParameter(d)) param = d;
    else if (
      ts.isBindingElement(d) &&
      ts.isObjectBindingPattern(d.parent) &&
      ts.isParameter(d.parent.parent)
    ) {
      param = d.parent.parent;
      prop = ((d.propertyName ?? d.name) as ts.Node).getText();
    }
    if (!param) return undefined;
    const cb = param.parent;
    if (
      !(ts.isArrowFunction(cb) || ts.isFunctionExpression(cb)) ||
      !ts.isCallExpression(cb.parent)
    )
      return undefined;
    const inner = unwrap(cb.parent.expression);
    if (
      !ts.isCallExpression(inner) ||
      !/^(describe|it|test)(\.\w+)*\.each$/.test(inner.expression.getText()) ||
      !inner.arguments[0]
    )
      return undefined;
    let table = unwrap(inner.arguments[0]);
    // const endpoints = [...]; describe.each(endpoints)(...)
    if (ts.isIdentifier(table)) {
      const td = declOf(table);
      const init = td && ts.isVariableDeclaration(td) ? initOf(td) : undefined;
      if (init) table = unwrap(init);
    }
    if (!ts.isArrayLiteralExpression(table)) return undefined;
    const index = cb.parameters.indexOf(param);
    const cells: ts.Expression[] = [];
    for (const row of table.elements.map(unwrap)) {
      if (ts.isArrayLiteralExpression(row)) {
        if (row.elements[index]) cells.push(row.elements[index]);
        continue;
      }
      if (index !== 0) continue;
      if (!prop) {
        cells.push(row);
        continue;
      }
      if (ts.isObjectLiteralExpression(row)) {
        const p = row.properties.find((x) => x.name?.getText() === prop);
        if (p && ts.isPropertyAssignment(p)) cells.push(p.initializer);
        else if (p && ts.isShorthandPropertyAssignment(p)) cells.push(p.name);
      }
    }
    return cells.length ? cells : undefined;
  }
  /** Origin of a table-driven parameter: the first row's cell, with the other rows as alternatives. */
  function eachTableOrigin(
    d: ts.Declaration,
    depth: number,
  ): Origin | undefined {
    const cells = eachTableCells(d);
    if (!cells) return undefined;
    const origins = cells
      .map((c) => originOf(c, depth + 1))
      .filter((o): o is Origin => !!o);
    if (!origins.length) return undefined;
    const [first, ...rest] = origins;
    return rest.length ? { ...first, alts: rest } : first;
  }
  /** The function a variable holds: `const run = () => ...`, or `const { getImage } = helper()` where helper returns `{ getImage: () => ... }`. */
  function heldFunction(
    d: ts.Declaration,
  ): ts.SignatureDeclaration | undefined {
    let v: ts.Expression | undefined;
    if (ts.isVariableDeclaration(d)) v = initOf(d);
    else if (
      ts.isBindingElement(d) &&
      ts.isObjectBindingPattern(d.parent) &&
      ts.isVariableDeclaration(d.parent.parent) &&
      d.parent.parent.initializer
    ) {
      const init = unwrap(d.parent.parent.initializer);
      if (ts.isCallExpression(init) && ts.isIdentifier(init.expression))
        v = localHelperPropertyNode(
          init.expression.text,
          ((d.propertyName ?? d.name) as ts.Node).getText(),
          d.getSourceFile(),
        );
    }
    const u = v ? unwrap(v) : undefined;
    return u && (ts.isArrowFunction(u) || ts.isFunctionExpression(u))
      ? u
      : undefined;
  }
  /** Origin of a local helper call through its return expression (not an object literal). */
  function localFnCallOrigin(
    call: ts.CallExpression,
    fn: ts.SignatureDeclaration,
    depth: number,
  ): Origin | undefined {
    if (depth > 16 || paramBindings.length > 4) return undefined;
    let ret: ts.Expression | undefined;
    const body = (fn as ts.FunctionLikeDeclaration).body;
    if (!body) return undefined;
    if (!ts.isBlock(body)) ret = unwrap(body as ts.Expression);
    else {
      const visit = (n: ts.Node) => {
        if (ret) return;
        if (ts.isReturnStatement(n) && n.expression) ret = unwrap(n.expression);
        else if (!ts.isFunctionLike(n)) ts.forEachChild(n, visit);
      };
      visit(body);
    }
    if (!ret || ts.isObjectLiteralExpression(ret)) return undefined;
    const bindings = new Map<ts.Declaration, ts.Expression>();
    fn.parameters.forEach((p, i) => {
      if (call.arguments[i]) bindings.set(p, call.arguments[i]);
    });
    paramBindings.push(bindings);
    try {
      return originOf(ret, depth + 1);
    } finally {
      paramBindings.pop();
    }
  }
  function localHelperProperty(
    name: string,
    prop: string,
    sf: ts.SourceFile,
    depth: number,
  ): Origin | undefined {
    const node = localHelperPropertyNode(name, prop, sf);
    return node ? originOf(node, depth) : undefined;
  }

  /** Regex source of a pattern argument: a literal, or new RegExp(string | template) with placeholders as wildcards. */
  function regexSource(arg: ts.Expression): string | undefined {
    const a = unwrap(arg);
    if (ts.isRegularExpressionLiteral(a))
      return a.getText().replace(/^\/|\/[a-z]*$/g, "");
    if (
      ts.isNewExpression(a) &&
      a.expression.getText() === "RegExp" &&
      a.arguments?.[0]
    ) {
      const p = unwrap(a.arguments[0]);
      if (ts.isStringLiteralLike(p)) return p.text;
      if (ts.isTemplateExpression(p))
        return (
          p.head.text +
          p.templateSpans.map((s) => ".*" + s.literal.text).join("")
        );
    }
    if (ts.isConditionalExpression(a)) {
      const l = regexSource(a.whenTrue);
      const r = regexSource(a.whenFalse);
      return l && r ? `${l}|${r}` : (l ?? r);
    }
    return undefined;
  }

  /** The production component behind a JSX tag, by declared name (matches inventory `owner`). */
  function componentOfTag(tag: ts.JsxTagNameExpression): string | undefined {
    const root = ts.isIdentifier(tag)
      ? tag
      : ts.isPropertyAccessExpression(tag)
        ? (rootOfExpr(tag) as ts.Identifier)
        : undefined;
    if (!root || !ts.isIdentifier(root) || /^[a-z]/.test(root.text))
      return undefined;
    const d = declOf(root);
    if (!d || !isProdFile(d.getSourceFile())) return undefined;
    if (ts.isFunctionDeclaration(d)) return d.name?.text;
    if (ts.isVariableDeclaration(d) && ts.isIdentifier(d.name))
      return d.name.text;
    return undefined;
  }
  function jsxComponentsIn(node: ts.Node, acc: Set<string>) {
    const visit = (n: ts.Node) => {
      if (ts.isJsxOpeningElement(n) || ts.isJsxSelfClosingElement(n)) {
        const c = componentOfTag(n.tagName);
        if (c) acc.add(c);
      }
      ts.forEachChild(n, visit);
    };
    visit(node);
  }
  /** Components rendered by a test, plus the production components their JSX renders (two levels). */
  function renderedComponents(roots: Set<string>): Set<string> {
    const all = new Set(roots);
    let frontier = [...roots];
    for (let depth = 0; depth < 2 && frontier.length; depth++) {
      const next: string[] = [];
      for (const name of frontier)
        for (const sf of allFiles) {
          if (!isProdFile(sf)) continue;
          const fn = functionByName(name, rel(sf));
          if (!fn) continue;
          const inner = new Set<string>();
          jsxComponentsIn(fn, inner);
          for (const c of inner)
            if (!all.has(c)) {
              all.add(c);
              next.push(c);
            }
        }
      frontier = next;
    }
    return all;
  }

  /** Diagnostic: assertion operands the origin model could not map, grouped by shape. */
  const unrecognized = new Map<string, number>();
  function noteUnrecognized(arg: ts.Expression, why: string) {
    const shape = arg
      .getText()
      .replace(/\s+/g, " ")
      .replace(/(['"`]).*?\1/g, "…")
      .replace(/\b\d+\b/g, "N")
      .slice(0, 45);
    const key = `${shape}  [${why}]`;
    unrecognized.set(key, (unrecognized.get(key) ?? 0) + 1);
  }

  function analyzeTestBody(
    fn: ts.Node,
    file: string,
    line: number,
    name: string,
    inert = false,
  ): StaticTest {
    const mockCounts = analyzeMockCounts(ts, fn, {
      declaration: declOf,
      location: comparisonLocation,
      nativeMock: nativeContextMock,
      nativePredicate: (call) => nativeComparison(call)?.predicate,
      globalConsole: (expr) => {
        const e = comparisonExpression(expr);
        const d = declOf(e);
        return (
          ts.isIdentifier(e) &&
          e.text === "console" &&
          (!d || d.getSourceFile().isDeclarationFile)
        );
      },
      production: (node) => isProdFile(node.getSourceFile()),
      site: (call) =>
        smallestSiteContaining(
          rel(call.getSourceFile()), call.getStart(), call.getEnd(),
        )?.id,
    });
    const observations: Observation[] = [];
    const sinks: SinkBinding[] = [];
    const rendered = new Set<string>();
    const pending: PendingOperand[] = [];
    /** the statements that compute `arg`: its own statement plus the declaring / assigning statements of the local variables it reads */
    const definingStatements = (arg: ts.Expression): string[] => {
      const sf = arg.getSourceFile();
      const key = (n: ts.Node) => {
        const { line, character } = sf.getLineAndCharacterOfPosition(
          n.getStart(sf),
        );
        return `${relative(root, sf.fileName)}:${line + 1}:${character + 1}`;
      };
      const statementOf = (n: ts.Node): ts.Node | undefined => {
        let cur: ts.Node | undefined = n;
        while (cur && !ts.isSourceFile(cur)) {
          if (
            ts.isStatement(cur) &&
            !ts.isBlock(cur) &&
            cur.parent &&
            (ts.isBlock(cur.parent) ||
              ts.isSourceFile(cur.parent) ||
              ts.isCaseClause(cur.parent) ||
              ts.isDefaultClause(cur.parent) ||
              ts.isModuleBlock(cur.parent))
          )
            return cur;
          cur = cur.parent;
        }
        return undefined;
      };
      const keys = new Set<string>();
      const own = statementOf(arg);
      if (own) keys.add(key(own));
      const seen = new Set<ts.Node>();
      const follow = (e: ts.Node, depth: number) => {
        if (depth > 3) return;
        const visit = (n: ts.Node) => {
          if (ts.isIdentifier(n)) {
            const d = declOf(n);
            if (
              d &&
              !seen.has(d) &&
              d.getSourceFile() === sf &&
              (ts.isVariableDeclaration(d) || ts.isBindingElement(d))
            ) {
              seen.add(d);
              const decl = ts.isBindingElement(d) ? d.parent.parent : d;
              const st = statementOf(decl);
              if (st) keys.add(key(st));
              const init = ts.isVariableDeclaration(decl)
                ? decl.initializer
                : undefined;
              if (init) follow(init, depth + 1);
              // `let x; ... x = compute()`: every assignment statement to the variable
              if (
                ts.isVariableDeclaration(decl) &&
                ts.isIdentifier(decl.name)
              ) {
                const sym = symbolOf(decl.name);
                const scan = (m: ts.Node) => {
                  if (
                    ts.isBinaryExpression(m) &&
                    m.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
                    ts.isIdentifier(m.left) &&
                    symbolOf(m.left) === sym
                  ) {
                    const st2 = statementOf(m);
                    if (st2) keys.add(key(st2));
                    follow(m.right, depth + 1);
                  }
                  ts.forEachChild(m, scan);
                };
                scan(sf);
              }
            }
          }
          ts.forEachChild(n, visit);
        };
        visit(e);
      };
      follow(arg, 0);
      return [...keys];
    };
    const sf = fn.getSourceFile();
    const where = (n: ts.Node, label: string) =>
      `${relative(root, sf.fileName)}:${sf.getLineAndCharacterOfPosition(n.getStart(sf)).line + 1} ${label}`;
    const visit = (node: ts.Node) => {
      if (ts.isCallExpression(node)) {
        const callee = unwrap(node.expression);
        // node:assert style: assert.method(actual, expected)
        let method: string | undefined;
        let strength: Strength | undefined;
        let actuals: ts.Expression[] = [];
        let expected: ts.Expression | undefined;
        let negative = false;
        let rejectsChain = false;
        if (
          ts.isPropertyAccessExpression(callee) &&
          callee.expression.getText() === "assert"
        )
          method = callee.name.text;
        else if (ts.isIdentifier(callee) && callee.text === "assert")
          method = "assert";
        if (method && ASSERT_STRENGTH[method]) {
          strength = ASSERT_STRENGTH[method];
          actuals = node.arguments.slice(0, 2);
          expected = node.arguments[1];
          negative = method === "doesNotMatch";
        } else if (ts.isPropertyAccessExpression(callee)) {
          // vitest/jest style: expect(actual)[.not][.resolves|.rejects].matcher(expected)
          method = undefined;
          let e: ts.Expression = unwrap(callee.expression);
          while (
            ts.isPropertyAccessExpression(e) &&
            ["not", "resolves", "rejects"].includes(e.name.text)
          ) {
            if (e.name.text === "not") negative = true;
            if (e.name.text === "rejects") rejectsChain = true;
            e = unwrap(e.expression);
          }
          if (
            ts.isCallExpression(e) &&
            ts.isIdentifier(unwrap(e.expression)) &&
            (unwrap(e.expression) as ts.Identifier).text === "expect" &&
            e.arguments[0] &&
            EXPECT_STRENGTH[callee.name.text]
          ) {
            method = callee.name.text;
            strength = EXPECT_STRENGTH[method];
            if (
              (method === "toThrow" || method === "toThrowError") &&
              !node.arguments[0]
            )
              strength = "presence";
            actuals = [e.arguments[0]];
            expected = node.arguments[0];
          }
        }
        // expect.objectContaining / expect.any / expect.anything inside the expected value: a subset match, not total
        if (
          strength === "total" &&
          expected &&
          /\bexpect\.(objectContaining|arrayContaining|anything|any|stringContaining|stringMatching|closeTo)\s*\(/.test(
            expected.getText(),
          )
        )
          strength = "value";
        if (method && strength) {
          const comparison = nativeComparison(node);
          pragmaCollector.register(
            node,
            method,
            staticTestKey(file, line, name),
            inert,
          );
          const s = strength;
          const pattern =
            expected &&
            [
              "match",
              "doesNotMatch",
              "toMatch",
              "toThrow",
              "toThrowError",
            ].includes(method)
              ? regexSource(expected)
              : undefined;
          const expectedLiteral =
            expected &&
            [
              "equal",
              "strictEqual",
              "notEqual",
              "notStrictEqual",
              "toBe",
              "toEqual",
              "toStrictEqual",
              "toThrow",
              "toThrowError",
            ].includes(method) &&
            ts.isStringLiteralLike(unwrap(expected))
              ? (unwrap(expected) as ts.StringLiteralLike).text
              : undefined;
          const expectedFragment =
            expected &&
            ["toContain", "toContainEqual", "toMatch"].includes(method) &&
            ts.isStringLiteralLike(unwrap(expected))
              ? (unwrap(expected) as ts.StringLiteralLike).text
              : undefined;
          const observesThrow =
            rejectsChain ||
            ["rejects", "throws", "toThrow", "toThrowError"].includes(method);
          for (const arg of actuals) {
            const o = originOf(arg);
            const unresolvedOperand = (shape: string) =>
              pending.push({
                assertionSource: `${relative(root, sf.fileName)}:${sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1}:${sf.getLineAndCharacterOfPosition(node.getStart(sf)).character + 1}`,
                assertionMethod: method,
                statements: definingStatements(arg),
                strength: RANK[s] > RANK.value ? "value" : s,
                negative: !!negative,
                rejects: observesThrow,
                where: where(node, method),
                shape: `${arg.getText().replace(/\s+/g, " ").slice(0, 40)} [${shape}]`,
              });
            if (!o) {
              noteUnrecognized(arg, "no origin");
              unresolvedOperand("no origin");
              continue;
            }
            const includes = o.facet?.startsWith("includes:")
              ? o.facet.slice(9)
              : undefined;
            const facetPattern = o.facet?.startsWith("pattern:")
              ? o.facet.slice(8)
              : undefined;
            const bs = boundariesOf(o);
            const mock = mockProjection(o);
            if (mock?.kind === "call-count")
              mock.countEvidence = mockCounts.checks.get(node) ?? {
                model: "node-sync-console-count-v1",
                status: "unresolved",
                reason: mockCounts.limitation ?? "unsupported-count-projection",
              };
            if (mock)
              unresolvedOperand(
                `mock ${mock.kind}; production call identity and projection dependence unresolved`,
              );
            if (!bs.length && o.kind !== "literal") {
              noteUnrecognized(arg, o.kind);
              unresolvedOperand(o.kind);
            }
            // rejects/throws: the function's throw sites are observed as well as (or instead of) its return
            if (observesThrow)
              for (const b of [...bs])
                if (b.boundary.startsWith("return:"))
                  bs.push({ boundary: "throw:" + b.boundary.slice(7) });
            // the whole call list of a sink: a call count, or `mock.calls` read as a whole or through a projection
            // (`calls.map(([p]) => p.blog_id)`, `calls.length`), as opposed to one call (`calls[0]`)
            const callsAt = o.path.indexOf("calls");
            const callList =
              method === "toHaveBeenCalledTimes" ||
              method === "toHaveBeenCalledOnce" ||
              (callsAt > 0 &&
                o.path[callsAt - 1] === "mock" &&
                !o.path.slice(callsAt + 1).includes("[]"));
            for (const b of bs) {
              const ob: Observation = {
                ...b,
                strength: s,
                where: where(node, method),
                assertionSource: `${relative(root, sf.fileName)}:${sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1}:${sf.getLineAndCharacterOfPosition(node.getStart(sf)).character + 1}`,
                assertionMethod: method,
                ...(mock ? { mock } : {}),
                ...(comparison ? { comparison } : {}),
              };
              if (!mock && callList && b.boundary.startsWith("sink:"))
                ob.callList = true;
              if (pattern) ob.pattern = pattern;
              else if (facetPattern) ob.pattern = facetPattern;
              if (includes)
                ob.fragment = includes; // a substring or prefix: pins every message that can contain it
              else if (expectedFragment) ob.fragment = expectedFragment;
              else if (expectedLiteral) ob.literal = expectedLiteral; // a whole message: pins the template that can produce it
              if (negative) ob.negative = true;
              observations.push(ob);
            }
          }
        }
        // implicit oracles: awaited reads that throw or time out
        if (ts.isAwaitExpression(node.parent)) {
          const o = originOf(node);
          if (o) {
            const bs = boundariesOf(o);
            let pattern: string | undefined;
            if (o.kind === "helper:launchGateway" && o.path.includes("ready()"))
              pattern = "Listening on port|Stdio server listening";
            if (
              o.kind === "helper:launchGateway" &&
              o.path.includes("waitFor()")
            ) {
              const lit = node.arguments[0]
                ?.getText()
                .match(/includes\((['"`])(.*?)\1\)/);
              pattern = lit
                ? lit[2].replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
                : undefined;
            }
            for (const b of bs)
              observations.push({
                ...b,
                strength: "presence",
                where: where(node, "await"),
                implicit: true,
                pattern,
              });
          }
        }
      }
      if (ts.isNewExpression(node) || ts.isCallExpression(node)) {
        const callee = unwrap(node.expression);
        if (ts.isIdentifier(callee)) {
          const d = declOf(callee);
          if (d && isProdFile(d.getSourceFile())) {
            const sig = checker.getResolvedSignature(node);
            const params = sig?.getParameters() ?? [];
            node.arguments?.forEach((arg, i) => {
              const paramName = params[i]?.name;
              if (!paramName) return;
              const paramDecl = params[i]?.valueDeclaration;
              // a destructured parameter `({ argv, logger })`: the property name is the binding the code uses
              const destructured =
                !!paramDecl &&
                ts.isParameter(paramDecl) &&
                ts.isObjectBindingPattern(paramDecl.name);
              // one sink may be wired through several members (logger.info and logger.error both push to `logs`)
              const found = new Map<
                string,
                { name: string; param: string; member?: string }
              >();
              const record = (name: string, path: string[]) => {
                const param = destructured ? path[0] : paramName;
                const member =
                  (destructured ? path.slice(1) : path).join(".") || undefined;
                if (param)
                  found.set(`${name}|${param}|${member ?? ""}`, {
                    name,
                    param,
                    member,
                  });
              };
              const scan = (n: ts.Node, path: string[], depth: number) => {
                if (depth > 5) return;
                if (
                  ts.isIdentifier(n) &&
                  !(ts.isPropertyAssignment(n.parent) && n.parent.name === n)
                ) {
                  const dd = declOf(n);
                  const ddInit =
                    dd && ts.isVariableDeclaration(dd) ? initOf(dd) : undefined;
                  if (dd && ts.isVariableDeclaration(dd) && ddInit) {
                    const init = unwrap(ddInit);
                    if (
                      ts.isArrayLiteralExpression(init) ||
                      ts.isObjectLiteralExpression(init) ||
                      isMockFactory(init)
                    ) {
                      record(n.text, path);
                      if (ts.isObjectLiteralExpression(init))
                        scan(init, path, depth + 1); // a logger object wiring other sinks
                      return;
                    }
                    if (
                      ts.isCallExpression(init) &&
                      ts.isIdentifier(unwrap(init.expression))
                    ) {
                      const h = localFunctionNode(
                        (unwrap(init.expression) as ts.Identifier).text,
                        sf,
                      );
                      // `table = createTable()` where the factory returns an object of mocks: the variable is the sink
                      const made = h ? returnedObject(h) : undefined;
                      if (made && containsMock(made)) {
                        record(n.text, path);
                        return;
                      }
                      if (h) scan(h, path, depth + 1);
                      return;
                    }
                    scan(ddInit, path, depth + 1);
                    return;
                  }
                  if (dd && ts.isBindingElement(dd)) {
                    // destructured from a local helper's returned object: scan what the helper put there
                    const decl = dd.parent.parent;
                    if (
                      ts.isVariableDeclaration(decl) &&
                      decl.initializer &&
                      ts.isCallExpression(unwrap(decl.initializer))
                    ) {
                      const c = unwrap(
                        (unwrap(decl.initializer) as ts.CallExpression)
                          .expression,
                      );
                      const prop = (
                        (dd.propertyName ?? dd.name) as ts.Node
                      ).getText();
                      if (ts.isIdentifier(c)) {
                        const expr = localHelperPropertyNode(c.text, prop, sf);
                        if (expr) scan(expr, path, depth + 1);
                      }
                    }
                    return;
                  }
                }
                if (ts.isPropertyAssignment(n) || ts.isMethodDeclaration(n)) {
                  const m = n.name.getText();
                  ts.forEachChild(n, (c) => scan(c, [...path, m], depth));
                  return;
                }
                if (ts.isShorthandPropertyAssignment(n)) {
                  scan(n.name, [...path, n.name.text], depth);
                  return;
                }
                ts.forEachChild(n, (c) => scan(c, path, depth));
              };
              scan(arg, [], 0);
              for (const b of found.values())
                sinks.push({
                  sink: "sink:" + b.name,
                  prodName: callee.text,
                  param: b.param,
                  member: b.member,
                });
            });
          }
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(fn);
    // local helper functions called from the body may create sinks or wire them into production code
    const helpers = new Set<ts.Node>();
    const collect = (n: ts.Node) => {
      if (ts.isCallExpression(n) && ts.isIdentifier(n.expression)) {
        const h = localFunctionNode(n.expression.text, sf);
        if (h && h !== fn) helpers.add(h);
      }
      ts.forEachChild(n, collect);
    };
    collect(fn);
    for (const h of helpers) visit(h);
    jsxComponentsIn(fn, rendered);
    for (const h of helpers) jsxComponentsIn(h, rendered);
    // `it.fails` / skipped tests are not oracles: keep them for linking, drop their observations
    return {
      file,
      line,
      name,
      observations: inert ? [] : observations,
      sinks,
      rendered: renderedComponents(rendered),
      pending: inert ? [] : pending,
    };
  }
  /** node:test `test(...)`, vitest/jest `it(...)`/`test(...)`, `it.each(table)(name, fn)`; `.fails`/`.skip`/`.todo` are not oracles. */
  function testDeclaration(
    node: ts.Node,
  ): { body: ts.Node; name: string; inert: boolean } | undefined {
    if (!ts.isCallExpression(node) || node.arguments.length < 2)
      return undefined;
    const body = node.arguments[node.arguments.length - 1];
    if (!(ts.isArrowFunction(body) || ts.isFunctionExpression(body)))
      return undefined;
    let callee: ts.Expression = unwrap(node.expression);
    if (ts.isCallExpression(callee)) callee = unwrap(callee.expression); // it.each(table)(...)
    const names = ts.isIdentifier(callee)
      ? [callee.text]
      : ts.isPropertyAccessExpression(callee)
        ? [callee.expression.getText(), callee.name.text]
        : [];
    if (!["test", "it"].includes(names[0])) return undefined;
    if (
      names[1] &&
      !["each", "only", "concurrent", "fails", "skip", "todo"].includes(
        names[1],
      )
    )
      return undefined;
    return {
      body,
      name: node.arguments[0].getText().slice(0, 60),
      inert: ["fails", "skip", "todo"].includes(names[1] ?? ""),
    };
  }
  for (const sf of allFiles) {
    if (!isTestFile(sf)) continue;
    moduleMocksByFile.set(rel(sf), collectModuleMocks(sf));
    globalSinksByFile.set(rel(sf), collectGlobalSinks(sf));
    const visit = (node: ts.Node) => {
      const decl = testDeclaration(node);
      if (decl) {
        const st = analyzeTestBody(
          decl.body,
          rel(sf),
          sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1,
          decl.name,
          decl.inert,
        );
        st.endLine = sf.getLineAndCharacterOfPosition(node.getEnd()).line + 1;
        const arg0 = (node as ts.CallExpression).arguments[0];
        st.title = ts.isStringLiteralLike(arg0)
          ? arg0.text
          : arg0.getText(sf).replace(/^[`'"]|[`'"]$/g, "");
        staticTests.push(st);
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }
  // Runtime line numbers come from ts-node's transpiled output (no source maps), so they
  // drift below the TypeScript lines. Order is preserved, so pair runtime call sites with
  // static test() calls by rank within each file; fall back to nearest-line when counts differ.
  const staticLink = new Map<string, StaticTest>();
  const linkWarnings: string[] = [];
  // Runners that report no test line (supercov): link by the lines of the test's assertion phases,
  // which lie inside exactly one test declaration, else by the leaf title.
  const staticById = new Map<string, StaticTest>();
  let linkedByPhases = 0;
  let linkedByTitle = 0;
  for (const rt of runtimeTests) {
    if (rt.line !== 0) continue;
    const statics = staticTests.filter((t) => t.file === rt.file);
    const lines = rt.phaseLines ?? [];
    const byPhases = lines.length
      ? statics
          .filter(
            (t) =>
              t.line <= lines[0] &&
              (t.endLine ?? t.line) >= lines[lines.length - 1],
          )
          .sort((a, b) => b.line - a.line)[0]
      : undefined;
    if (byPhases) {
      staticById.set(rt.id, byPhases);
      linkedByPhases++;
      continue;
    }
    const title = rt.title ?? rt.name;
    const byTitle = statics.find(
      (t) =>
        t.title !== undefined &&
        (t.title === title ||
          (/[$%]/.test(t.title) &&
            title.startsWith(t.title.split(/[$%]/)[0].trimEnd()))),
    );
    if (byTitle) {
      staticById.set(rt.id, byTitle);
      linkedByTitle++;
    } else linkWarnings.push(`${rt.file}: no static test for "${title}"`);
  }
  for (const file of new Set(
    runtimeTests.filter((t) => t.line !== 0).map((t) => t.file),
  )) {
    const runtimeLines = [
      ...new Set(
        runtimeTests
          .filter((t) => t.file === file && t.line !== 0)
          .map((t) => t.line),
      ),
    ].sort((a, b) => a - b);
    const statics = staticTests
      .filter((t) => t.file === file)
      .sort((a, b) => a.line - b.line);
    // exact source lines (a runner that maps locations) win; otherwise pair by rank
    if (
      runtimeLines.length &&
      runtimeLines.every((l) => statics.some((s) => s.line === l))
    )
      runtimeLines.forEach((l) =>
        staticLink.set(`${file}:${l}`, statics.find((s) => s.line === l)!),
      );
    else if (runtimeLines.length === statics.length)
      runtimeLines.forEach((l, i) =>
        staticLink.set(`${file}:${l}`, statics[i]),
      );
    else {
      linkWarnings.push(
        `${file}: ${runtimeLines.length} runtime call sites vs ${statics.length} static tests; using nearest-line fallback`,
      );
      for (const l of runtimeLines) {
        const st =
          statics
            .filter((t) => t.line <= l)
            .sort((a, b) => b.line - a.line)[0] ?? statics[0];
        if (st) staticLink.set(`${file}:${l}`, st);
      }
    }
  }
  const staticFor = (rt: RuntimeTest) =>
    staticById.get(rt.id) ?? staticLink.get(`${rt.file}:${rt.line}`);

  interface DecisionFacts {
    carrier?: string;
    /** value-position expression not inside a site: the sites its value flows to */
    valueFlow?: string[];
    /** sites and carriers of the branch the true outcome takes */
    then?: string[];
    /** the same for the false outcome; null means there is no else branch (the absence case) */
    else?: string[] | null;
    /** effects the function would go on to perform if an early-exit branch fell through */
    earlyExitDownstream?: string[];
    /** body sites a `continue` or `break` decides the execution of */
    loopBody?: string[];
    /** state writes in the branch, with the sites that observe the state they write */
    defaultKept?: {
      write: string;
      dependents: { site: string; label: string }[];
    }[];
    /** ternary between two pre-built objects: sites only one branch reaches */
    objectValued?: { onlyA: string[]; onlyB: string[] };
  }
  const decisionFacts = new Map<string, DecisionFacts>();

  function classByName(
    name: string,
    file: string,
  ): ts.ClassDeclaration | undefined {
    const sf = srcByFile.get(file);
    let found: ts.ClassDeclaration | undefined;
    const visit = (n: ts.Node) => {
      if (found) return;
      if (ts.isClassDeclaration(n) && n.name?.text === name) {
        found = n;
        return;
      }
      ts.forEachChild(n, visit);
    };
    if (sf) visit(sf);
    return found;
  }
  /** Functions passed (directly or as an object property) for parameter `paramName` at call sites of `fn`. */
  function callbackTargets(
    fn: ts.SignatureDeclaration | ts.ClassDeclaration,
    paramName: string,
  ): ts.Node[] {
    const params = paramsOf(fn);
    let index = -1;
    let path: string[] = [];
    params.forEach((p, i) => {
      if (ts.isIdentifier(p.name) && p.name.text === paramName) index = i;
      else if (ts.isObjectBindingPattern(p.name)) {
        const el = p.name.elements.find((e) => e.name.getText() === paramName);
        if (el) {
          index = i;
          path = [((el.propertyName ?? el.name) as ts.Node).getText()];
        }
      }
    });
    if (index < 0) return [];
    const targets: ts.Node[] = [];
    const asFunction = (arg: ts.Expression | undefined) => {
      if (!arg) return;
      const u = unwrap(arg);
      if (ts.isArrowFunction(u) || ts.isFunctionExpression(u)) targets.push(u);
      else if (ts.isIdentifier(u)) {
        const d = declOf(u);
        if (d && ts.isFunctionDeclaration(d)) targets.push(d);
        else if (
          d &&
          ts.isVariableDeclaration(d) &&
          d.initializer &&
          (ts.isArrowFunction(unwrap(d.initializer)) ||
            ts.isFunctionExpression(unwrap(d.initializer)))
        )
          targets.push(unwrap(d.initializer));
      }
    };
    for (const sf of allFiles) {
      if (!isProdFile(sf)) continue;
      const visit = (n: ts.Node) => {
        if (
          (ts.isCallExpression(n) || ts.isNewExpression(n)) &&
          projectCallee(n.expression) === fn
        ) {
          let arg: ts.Expression | undefined = n.arguments?.[index];
          if (arg && path.length && ts.isObjectLiteralExpression(unwrap(arg))) {
            const prop = (
              unwrap(arg) as ts.ObjectLiteralExpression
            ).properties.find((p) => p.name?.getText() === path[0]);
            arg =
              prop && ts.isPropertyAssignment(prop)
                ? prop.initializer
                : prop && ts.isShorthandPropertyAssignment(prop)
                  ? prop.name
                  : undefined;
          }
          asFunction(arg);
        }
        ts.forEachChild(n, visit);
      };
      visit(sf);
    }
    return targets;
  }

  /** All boundaries an effect site is observable through: direct, plus inter-procedural flow. */
  function allBoundaries(s: Site): { bounds: Boundary[]; reached: Site[] } {
    const bounds = directBoundaries(s);
    const reached: Site[] = [];
    if (s.category === "return" || s.category === "callback-return") {
      const fn = functionByName(s.owner, s.file);
      if (fn) {
        const flow = flowFromReturn(fn);
        bounds.push(...flow.boundaries);
        for (const id of flow.sites) {
          const r = sites.find((x) => x.id === id);
          if (r && r.id !== s.id) reached.push(r);
        }
      }
    }
    // a throw escapes to the callers that do not catch it: `rejects.toBe(err)` on the public function observes
    // a rethrow deep inside a helper
    if (s.category === "throw") {
      const fn = functionByName(s.owner, s.file);
      if (fn) bounds.push(...throwsThrough(fn, 0, new Set()));
    }
    // calling a callback parameter runs whatever function the caller passed: its sites are reached
    if (
      s.category === "external-call" &&
      (s.note === "param" || s.note === "this-callback") &&
      s.chain
    ) {
      const paramName = s.chain[0] === "this" ? s.chain[1] : s.chain[0];
      const holder =
        s.note === "this-callback"
          ? classByName(s.owner.split(".")[0], s.file)
          : functionByName(s.owner, s.file);
      if (holder && paramName) {
        for (const t of callbackTargets(holder, paramName)) {
          const tf = rel(t.getSourceFile());
          for (const e of effectSites)
            if (
              e.file === tf &&
              e.pos >= t.getStart() &&
              e.endPos <= t.getEnd()
            )
              reached.push(e);
        }
      }
    }
    return { bounds, reached };
  }
  /**
   * `throw:<caller>` for each project caller an exception escapes to: the call sits outside any try block, is not
   * chained with `.catch`/`.then`, and (for an async callee) is awaited or returned so the rejection propagates.
   * Follows the escape upward through the named owners of those calls.
   */
  function throwsThrough(
    fn: ts.SignatureDeclaration,
    depth: number,
    seen: Set<string>,
  ): Boundary[] {
    const key = fnKey(fn);
    if (depth > 4 || seen.has(key)) return [];
    seen.add(key);
    const out: Boundary[] = [];
    const isAsync = !!(
      ts.canHaveModifiers(fn) &&
      ts.getModifiers(fn)?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword)
    );
    for (const c of callSitesOf(fn)) {
      let guarded = false;
      let awaited = false;
      let returned = false;
      let node: ts.Node = c;
      while (node.parent && !ts.isFunctionLike(node.parent)) {
        const p: ts.Node = node.parent;
        if (ts.isTryStatement(p) && p.tryBlock === node) guarded = true;
        if (ts.isAwaitExpression(p)) awaited = true;
        if (ts.isReturnStatement(p)) returned = true;
        if (
          ts.isPropertyAccessExpression(p) &&
          p.expression === node &&
          (p.name.text === "catch" || p.name.text === "then")
        )
          guarded = true;
        node = p;
      }
      if (ts.isArrowFunction(node.parent) && node.parent.body === node)
        returned = true;
      if (guarded || (isAsync && !awaited && !returned)) continue;
      const owner = ownerOf(c);
      const name = nameOfFunction(owner);
      if (!owner || !name) continue;
      out.push({ boundary: "throw:" + name, via: `escapes ${name}` });
      out.push(...throwsThrough(owner, depth + 1, seen));
    }
    return out;
  }

  /**
   * Unknown operand dependence is a limit, not proof of an absent assertion.
   * Synchronous statement attribution can identify a possible relationship but
   * cannot exclude one across an await, pipe capture, or another async boundary.
   * A passing unmodeled assertion therefore leaves dependence unresolved for
   * sites covered by that test. This only changes the reason for an unresolved
   * candidate: it never supplies an observation, strength, or positive test link.
   */
  function unmodelledOperands(s: Site, covering: RuntimeTest[]): string[] {
    buildFunctionIndex();
    const shapes = new Set<string>();
    for (const rt of covering) {
      const byStatement = runtimeStatements.get(rt.id);
      const st = staticFor(rt);
      if (!st) continue;
      const phases = runtimePhases.get(rt.id);
      for (const p of st.pending) {
        if (phases) {
          if (
            !assertionWitnessIssue(phases, p.assertionSource, p.assertionMethod)
          )
            shapes.add(
              `${p.where}: ${p.shape} (passing assertion in covering test ${rt.id}; operand dependence unresolved)`,
            );
          // A known failed, mixed, incomplete or unexecuted call cannot supply
          // this passing-operand limit. Missing witness transport is separately
          // reported by witnessIssues. Legacy statement evidence remains below.
          continue;
        }
        if (!byStatement) continue;
        for (const pos of p.statements) {
          const attribution = byStatement[pos];
          if (!attribution) continue;
          const entered =
            attribution.fns.some(
              (key) => functionIndex.get(key)?.name === s.owner,
            ) ||
            (s.kind === "decision" && attribution.decs.some((d) => d === s.id));
          if (entered) shapes.add(p.shape);
        }
      }
    }
    return [...shapes];
  }

  function isObjectValuedReturn(s: Site): boolean {
    const node = siteNodes.get(s.id);
    if (!node || !ts.isReturnStatement(node) || !node.expression) return false;
    const objectish = (e: ts.Expression): boolean => {
      const u = unwrap(e);
      if (ts.isConditionalExpression(u))
        return objectish(u.whenTrue) && objectish(u.whenFalse);
      if (!ts.isIdentifier(u)) return false;
      const d = declOf(u);
      if (!d || !ts.isVariableDeclaration(d) || !d.initializer) return false;
      const init = unwrap(d.initializer);
      return (
        ts.isObjectLiteralExpression(init) ||
        ts.isArrowFunction(init) ||
        ts.isFunctionExpression(init)
      );
    };
    return objectish(node.expression);
  }

  function terminates(stmt: ts.Statement): boolean {
    if (
      ts.isReturnStatement(stmt) ||
      ts.isThrowStatement(stmt) ||
      ts.isContinueStatement(stmt) ||
      ts.isBreakStatement(stmt)
    )
      return true;
    if (ts.isBlock(stmt) && stmt.statements.length)
      return terminates(stmt.statements[stmt.statements.length - 1]);
    return false;
  }
  // ---------------------------------------------------------------------------
  // Execution records locate functions but do not establish returned-value or
  // thrown-value dependence. Keep that evidence separate from value observations.
  // ---------------------------------------------------------------------------
  const functionIndex = new Map<string, EnteredFn>();
  let functionIndexBuilt = false;
  function buildFunctionIndex() {
    if (functionIndexBuilt) return;
    functionIndexBuilt = true;
    for (const sf of allFiles) {
      if (!isProdFile(sf)) continue;
      const file = rel(sf);
      const visit = (n: ts.Node) => {
        if (
          ts.isFunctionLike(n) &&
          !ts.isMethodSignature(n) &&
          !ts.isFunctionTypeNode(n)
        ) {
          const name = nameOfFunction(n);
          if (name) {
            const { line, character } = sf.getLineAndCharacterOfPosition(
              n.getStart(sf),
            );
            functionIndex.set(`${file}:${line + 1}:${character + 1}`, {
              name,
              node: n as ts.SignatureDeclaration,
            });
          }
        }
        ts.forEachChild(n, visit);
      };
      visit(sf);
    }
  }
  type EnteredFn = {
    name: string;
    node: ts.SignatureDeclaration;
  };
  /** Production call sites of a function (direct calls only, the same lookup flowFromReturn uses). */
  const callSitesCache = new Map<string, ts.CallExpression[]>();
  function callSitesOf(fn: ts.SignatureDeclaration): ts.CallExpression[] {
    const key = fnKey(fn);
    const cached = callSitesCache.get(key);
    if (cached) return cached;
    const calls: ts.CallExpression[] = [];
    for (const sf of allFiles) {
      if (!isProdFile(sf)) continue;
      const visit = (x: ts.Node) => {
        if (ts.isCallExpression(x) && projectCallee(x.expression) === fn)
          calls.push(x);
        ts.forEachChild(x, visit);
      };
      visit(sf);
    }
    callSitesCache.set(key, calls);
    return calls;
  }
  /** `return …`, `throw …`, or a block that ends in one: the branch leaves the function. */
  function exitsEarly(stmt: ts.Statement): boolean {
    if (ts.isReturnStatement(stmt) || ts.isThrowStatement(stmt)) return true;
    if (ts.isBlock(stmt)) {
      const last = stmt.statements[stmt.statements.length - 1];
      return (
        !!last && (ts.isReturnStatement(last) || ts.isThrowStatement(last))
      );
    }
    return false;
  }

  function sitesInRange(file: string, range: [number, number]): string[] {
    return effectSites
      .filter(
        (e) => e.file === file && e.pos >= range[0] && e.endPos <= range[1],
      )
      .map((e) => e.id);
  }
  /** Effect sites that a branch reaches through values it assigns or returns (local flow). */
  function carriersOfRange(
    sf: ts.SourceFile,
    range: [number, number],
  ): Set<string> {
    const acc: FlowResult = { boundaries: [], sites: new Set() };
    const visited = new Set<string>();
    const visit = (n: ts.Node) => {
      if (n.getStart(sf) >= range[0] && n.getEnd() <= range[1]) {
        if (
          ts.isBinaryExpression(n) &&
          n.operatorToken.kind === ts.SyntaxKind.EqualsToken
        )
          propagateValue(n.right, acc, visited, 3);
        if (ts.isReturnStatement(n) && n.expression)
          propagateValue(n.expression, acc, visited, 3);
        if (ts.isVariableDeclaration(n) && n.initializer)
          propagateValue(n.initializer, acc, visited, 3);
      }
      ts.forEachChild(n, visit);
    };
    visit(sf);
    // boundaries reached without a site (external sinks) count as a pseudo-site: represent by owner return
    return acc.sites;
  }

  const MUTATORS = new Set([
    "set",
    "delete",
    "clear",
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "add",
  ]);
  function rootOfExpr(e: ts.Expression): ts.Expression {
    let cur = unwrap(e);
    for (;;) {
      if (
        ts.isPropertyAccessExpression(cur) ||
        ts.isElementAccessExpression(cur) ||
        ts.isCallExpression(cur)
      ) {
        cur = unwrap(cur.expression);
        continue;
      }
      return cur;
    }
  }
  function chainOf(e: ts.Expression): string[] {
    const names: string[] = [];
    let cur = unwrap(e);
    for (;;) {
      if (ts.isPropertyAccessExpression(cur)) {
        names.unshift(cur.name.text);
        cur = unwrap(cur.expression);
        continue;
      }
      if (ts.isElementAccessExpression(cur) || ts.isCallExpression(cur)) {
        cur = unwrap(cur.expression);
        continue;
      }
      break;
    }
    if (ts.isIdentifier(cur)) names.unshift(cur.text);
    else if (cur.kind === ts.SyntaxKind.ThisKeyword) names.unshift("this");
    return names;
  }
  function isWriteTarget(n: ts.Node): boolean {
    const p = n.parent;
    if (!p) return false;
    if (
      ts.isBinaryExpression(p) &&
      p.left === n &&
      p.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
      p.operatorToken.kind <= ts.SyntaxKind.LastAssignment
    )
      return true;
    if (
      (ts.isPrefixUnaryExpression(p) || ts.isPostfixUnaryExpression(p)) &&
      p.operand === n
    )
      return true;
    if (ts.isDeleteExpression(p)) return true;
    if (ts.isElementAccessExpression(p) && p.expression === n)
      return isWriteTarget(p);
    if (ts.isPropertyAccessExpression(p) && p.expression === n) {
      if (
        ts.isCallExpression(p.parent) &&
        p.parent.expression === p &&
        MUTATORS.has(p.name.text)
      )
        return true;
      return isWriteTarget(p);
    }
    return false;
  }
  type StateKey = {
    kind: "field" | "var" | "prop";
    name: string;
    sym?: ts.Symbol;
  };
  function stateKey(s: Site, node: ts.Node): StateKey | undefined {
    let target: ts.Expression | undefined;
    if (ts.isBinaryExpression(node)) target = node.left;
    else if (
      ts.isPrefixUnaryExpression(node) ||
      ts.isPostfixUnaryExpression(node)
    )
      target = node.operand;
    else if (ts.isDeleteExpression(node)) target = node.expression;
    else if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(unwrap(node.expression))
    )
      target = (unwrap(node.expression) as ts.PropertyAccessExpression)
        .expression;
    if (!target) return undefined;
    const t = unwrap(target);
    const root = rootOfExpr(t);
    if (root.kind === ts.SyntaxKind.ThisKeyword) {
      const names = chainOf(t);
      return names[1] ? { kind: "field", name: names[1] } : undefined;
    }
    if (ts.isIdentifier(root)) {
      if (t === root)
        return { kind: "var", name: root.text, sym: symbolOf(root) };
      if (/alias/.test(s.note ?? "") && ts.isPropertyAccessExpression(t))
        return { kind: "prop", name: t.name.text };
      return { kind: "var", name: root.text, sym: symbolOf(root) };
    }
    return undefined;
  }
  function readsOf(key: StateKey, node: ts.Node, sf: ts.SourceFile): ts.Node[] {
    let scope: ts.Node = sf;
    if (key.kind === "field" || key.kind === "prop") {
      let c: ts.Node | undefined = node;
      while (c && !ts.isClassDeclaration(c)) c = c.parent;
      scope = c ?? ownerOf(node) ?? sf;
    } else if (key.sym) {
      const d = key.sym.valueDeclaration ?? key.sym.declarations?.[0];
      scope = d ? (enclosingFunction(d) ?? sf) : sf;
    }
    const reads: ts.Node[] = [];
    const inside = (n: ts.Node) =>
      n.getStart(sf) >= node.getStart(sf) && n.getEnd() <= node.getEnd();
    const visit = (n: ts.Node) => {
      if (!inside(n)) {
        if (
          key.kind === "field" &&
          ts.isPropertyAccessExpression(n) &&
          n.expression.kind === ts.SyntaxKind.ThisKeyword &&
          n.name.text === key.name &&
          !isWriteTarget(n)
        )
          reads.push(n);
        else if (
          key.kind === "prop" &&
          ts.isPropertyAccessExpression(n) &&
          n.name.text === key.name &&
          !isWriteTarget(n)
        )
          reads.push(n);
        else if (
          key.kind === "var" &&
          ts.isIdentifier(n) &&
          key.sym &&
          symbolOf(n) === key.sym &&
          !isWriteTarget(n) &&
          !(ts.isVariableDeclaration(n.parent) && n.parent.name === n) &&
          !(ts.isParameter(n.parent) && n.parent.name === n)
        )
          reads.push(n);
      }
      ts.forEachChild(n, visit);
    };
    visit(scope);
    return reads;
  }
  function dependentsOfRead(
    r: ts.Node,
    file: string,
    sf: ts.SourceFile,
    depth = 0,
  ): Site[] {
    const out: Site[] = [];
    const start = r.getStart(sf);
    const end = r.getEnd();
    const atom = sites
      .filter(
        (x) =>
          x.kind === "decision" &&
          x.file === file &&
          x.pos <= start &&
          x.endPos >= end,
      )
      .sort((a, b) => a.endPos - a.pos - (b.endPos - b.pos))[0];
    if (atom) out.push(atom);
    const eff = smallestSiteContaining(file, start, end);
    if (eff) out.push(eff);
    if (depth < 1) {
      let p: ts.Node | undefined = r.parent;
      while (
        p &&
        !ts.isStatement(p) &&
        !ts.isVariableDeclaration(p) &&
        !ts.isFunctionLike(p)
      )
        p = p.parent;
      if (p && ts.isVariableDeclaration(p) && ts.isIdentifier(p.name)) {
        const decl = p;
        const sym = symbolOf(decl.name);
        const scope = enclosingFunction(decl) ?? sf;
        const visit = (n: ts.Node) => {
          if (
            ts.isIdentifier(n) &&
            n !== decl.name &&
            sym &&
            symbolOf(n) === sym
          )
            out.push(...dependentsOfRead(n, file, sf, depth + 1));
          ts.forEachChild(n, visit);
        };
        visit(scope);
      }
    }
    return out;
  }

  function callbackSitesOf(
    call: ts.CallExpression,
    file: string,
    sf: ts.SourceFile,
  ): Site[] {
    const cb = call.arguments.find(
      (a) => ts.isArrowFunction(a) || ts.isFunctionExpression(a),
    );
    return cb
      ? effectSites.filter(
          (e) =>
            e.file === file &&
            e.pos >= cb.getStart(sf) &&
            e.endPos <= cb.getEnd(),
        )
      : [];
  }
  /** The sites that depend on reads of the state a state-write site writes (undefined when the key is unknown). */
  function stateWriteDeps(s: Site):
    | {
        site: Site;
        label: string;
        strength?: Strength;
        requiresTotal?: string[];
      }[]
    | undefined {
    const node = siteNodes.get(s.id);
    const sf = srcByFile.get(s.file);
    if (!node || !sf) return undefined;
    const key = stateKey(s, node);
    if (!key) return undefined;
    const deps: { site: Site; label: string }[] = [];
    for (const r of readsOf(key, node, sf))
      for (const d of dependentsOfRead(r, s.file, sf))
        deps.push({
          site: d,
          label: `read ${r.getText(sf).replace(/\s+/g, " ").slice(0, 30)}`,
        });
    return deps;
  }
  /**
   * The sites through which an internal effect becomes observable: a timer through its callback's sites, a
   * cancelled timer through a callback with a total sink, a project method call through the method's sites,
   * a state write through the sites that depend on reads of that state. Undefined when the site's state key
   * cannot be determined, which is the honest "no derivation possible".
   */
  function deriveDeps(s: Site):
    | {
        site: Site;
        label: string;
        strength?: Strength;
        requiresTotal?: string[];
      }[]
    | undefined {
    const node = siteNodes.get(s.id);
    const sf = srcByFile.get(s.file);
    if (!node || !sf) return undefined;
    const deps: {
      site: Site;
      label: string;
      strength?: Strength;
      requiresTotal?: string[];
    }[] = [];
    if (s.category === "schedule" && ts.isCallExpression(node)) {
      if (s.method === "setTimeout" || s.method === "setInterval") {
        for (const e of callbackSitesOf(node, s.file, sf))
          deps.push({ site: e, label: "timer callback" });
      } else {
        // cancelling a timer is observable only as the callback not running: needs a total sink assertion on that callback
        for (const e of effectSites) {
          if (
            e.file !== s.file ||
            e.category !== "schedule" ||
            e.method !== "setTimeout" ||
            e.owner.split(".")[0] !== s.owner.split(".")[0]
          )
            continue;
          const en = siteNodes.get(e.id);
          if (!en || !ts.isCallExpression(en)) continue;
          // A dependency candidate, not a verdict. The engine checks whether any
          // of these callback sites has total evidence at the derivation stage.
          deps.push({
            site: e,
            label: "cancelled timer with total sink",
            strength: "value",
            requiresTotal: callbackSitesOf(en, s.file, sf).map((c) => c.id),
          });
        }
      }
    } else if (s.category === "state-call" && ts.isCallExpression(node)) {
      const callee = unwrap(node.expression);
      if (ts.isPropertyAccessExpression(callee)) {
        const d = declOf(callee.name);
        if (d && ts.isMethodDeclaration(d)) {
          const mf = rel(d.getSourceFile());
          for (const e of sites)
            if (
              e.file === mf &&
              e.pos >= d.getStart() &&
              e.endPos <= d.getEnd()
            )
              deps.push({ site: e, label: `method ${callee.name.text}` });
        }
      }
    } else if (s.category === "state-write") {
      const sd = stateWriteDeps(s);
      if (!sd) return undefined;
      deps.push(...sd);
    }
    return deps;
  }

  /** Pure syntax/flow facts. Whether a witness is needed and sufficient belongs to the Rust join. */
  function analyzeDecision(s: Site): DecisionFacts {
    const sf = srcByFile.get(s.file)!;
    const atom = siteNodes.get(s.id);
    let n: ts.Node | undefined = atom;
    const context = (x: ts.Node) =>
      ts.isIfStatement(x) ||
      ts.isConditionalExpression(x) ||
      ts.isWhileStatement(x) ||
      ts.isDoStatement(x) ||
      ts.isForStatement(x);
    const logical = (x: ts.Node) =>
      ts.isBinaryExpression(x) &&
      ["&&", "||", "??"].includes(x.operatorToken.getText(sf));
    const logicalTop = (x: ts.Node) =>
      logical(x) &&
      !logical(x.parent) &&
      !(
        context(x.parent) &&
        (ts.isIfStatement(x.parent) ||
        ts.isWhileStatement(x.parent) ||
        ts.isDoStatement(x.parent)
          ? x.parent.expression === x
          : ts.isConditionalExpression(x.parent) || ts.isForStatement(x.parent)
            ? x.parent.condition === x
            : false)
      );
    while (n && !(context(n) || logicalTop(n))) n = n.parent;
    if (!n) return {};
    const facts: DecisionFacts = {};
    let thenRange: [number, number] | undefined;
    let elseRange: [number, number] | undefined;
    let carrier: Site | undefined;
    if (ts.isIfStatement(n)) {
      thenRange = [n.thenStatement.getStart(sf), n.thenStatement.getEnd()];
      if (n.elseStatement)
        elseRange = [n.elseStatement.getStart(sf), n.elseStatement.getEnd()];
      else if (terminates(n.thenStatement) && ts.isBlock(n.parent))
        elseRange = [n.getEnd(), n.parent.getEnd()];
    } else if (
      ts.isWhileStatement(n) ||
      ts.isDoStatement(n) ||
      ts.isForStatement(n)
    ) {
      thenRange = [n.statement.getStart(sf), n.statement.getEnd()];
      if (ts.isBlock(n.parent)) elseRange = [n.getEnd(), n.parent.getEnd()];
    } else carrier = smallestSiteContaining(s.file, n.getStart(sf), n.getEnd());

    const objectValued =
      ts.isConditionalExpression(n) &&
      [n.whenTrue, n.whenFalse].every((b) => {
        const u = unwrap(b);
        if (!ts.isIdentifier(u)) return false;
        const d = declOf(u);
        return (
          !!d &&
          ts.isVariableDeclaration(d) &&
          !!d.initializer &&
          (ts.isObjectLiteralExpression(unwrap(d.initializer)) ||
            ts.isArrowFunction(unwrap(d.initializer)) ||
            ts.isFunctionExpression(unwrap(d.initializer)) ||
            ts.isCallExpression(unwrap(d.initializer)))
        );
      });
    if (objectValued && ts.isConditionalExpression(n)) {
      const branchSites = (b: ts.Expression): Set<string> => {
        const ids = new Set<string>();
        const d = declOf(unwrap(b) as ts.Identifier);
        const init =
          d && ts.isVariableDeclaration(d) ? d.initializer : undefined;
        if (!init) return ids;
        const add = (from: number, to: number) => {
          for (const e of effectSites)
            if (e.file === s.file && e.pos >= from && e.endPos <= to)
              ids.add(e.id);
        };
        add(init.getStart(sf), init.getEnd());
        const visit = (x: ts.Node) => {
          if (ts.isCallExpression(x) && ts.isIdentifier(x.expression)) {
            const f = localFunctionNode(x.expression.text, sf);
            if (f) add(f.getStart(sf), f.getEnd());
          }
          ts.forEachChild(x, visit);
        };
        visit(init);
        return ids;
      };
      const a = branchSites(n.whenTrue),
        b = branchSites(n.whenFalse);
      facts.objectValued = {
        onlyA: [...a].filter((x) => !b.has(x)),
        onlyB: [...b].filter((x) => !a.has(x)),
      };
      return facts;
    }
    if (carrier) {
      facts.carrier = carrier.id;
      return facts;
    }
    if (!thenRange) {
      const acc: FlowResult = { boundaries: [], sites: new Set() };
      propagateValue(n, acc, new Set(), 3);
      facts.valueFlow = [...acc.sites];
      return facts;
    }
    const thenIds = new Set(sitesInRange(s.file, thenRange));
    for (const id of carriersOfRange(sf, thenRange)) thenIds.add(id);
    facts.then = [...thenIds];
    facts.else = null;
    if (elseRange) {
      const ids = new Set(sitesInRange(s.file, elseRange));
      for (const id of carriersOfRange(sf, elseRange)) ids.add(id);
      facts.else = [...ids];
    }
    const haveOutcomes =
      testsWithOutcome(s, true) !== undefined &&
      testsWithOutcome(s, false) !== undefined;
    if (haveOutcomes && ts.isIfStatement(n) && exitsEarly(n.thenStatement)) {
      const fnNode = atom ? enclosingFunction(atom) : undefined;
      const fnEnd = fnNode ? fnNode.getEnd() : sf.getEnd();
      const downstream = effectSites.filter(
        (d) =>
          d.file === s.file &&
          d.pos >= thenRange![1] &&
          d.endPos <= fnEnd &&
          d.category !== "log",
      );
      if (fnNode)
        for (const c of callSitesOf(fnNode)) {
          const cf = rel(c.getSourceFile());
          const callerFn = enclosingFunction(c);
          const callerEnd = callerFn
            ? callerFn.getEnd()
            : c.getSourceFile().getEnd();
          for (const d of effectSites)
            if (
              d.file === cf &&
              d.pos >= c.getEnd() &&
              d.endPos <= callerEnd &&
              d.category !== "log"
            )
              downstream.push(d);
        }
      // The prototype omitted this when a branch was already strong. Retain the
      // source fact here; the engine already makes that verdict-dependent choice.
      facts.earlyExitDownstream = downstream.map((d) => d.id);
    }
    const loopControl = (st: ts.Statement): boolean =>
      ts.isContinueStatement(st) ||
      ts.isBreakStatement(st) ||
      (ts.isBlock(st) &&
        st.statements.length > 0 &&
        loopControl(st.statements[st.statements.length - 1]));
    if (haveOutcomes && ts.isIfStatement(n) && loopControl(n.thenStatement)) {
      let loop: ts.Node | undefined = n.parent;
      while (
        loop &&
        !ts.isIterationStatement(loop, false) &&
        !ts.isFunctionLike(loop) &&
        !ts.isSwitchStatement(loop)
      )
        loop = loop.parent;
      if (loop && ts.isIterationStatement(loop, false)) {
        const body = loop.statement;
        const last = (st: ts.Statement): ts.Statement =>
          ts.isBlock(st) && st.statements.length
            ? last(st.statements[st.statements.length - 1])
            : st;
        const from = ts.isBreakStatement(last(n.thenStatement))
          ? body.getStart(sf)
          : n.getEnd();
        const bodySites = effectSites.filter(
          (d) =>
            d.file === s.file &&
            d.pos >= from &&
            d.endPos <= body.getEnd() &&
            d.category !== "log",
        );
        const calleeSites = (node: ts.Node): Site[] => {
          const found: Site[] = [];
          const visit = (x: ts.Node) => {
            if (ts.isCallExpression(x)) {
              const target = projectCallee(x.expression);
              if (target && !ts.isClassDeclaration(target)) {
                const tf = rel(target.getSourceFile());
                for (const d of effectSites)
                  if (
                    d.file === tf &&
                    d.pos >= target.getStart() &&
                    d.endPos <= target.getEnd() &&
                    d.category !== "log"
                  )
                    found.push(d);
              }
            }
            ts.forEachChild(x, visit);
          };
          visit(node);
          return found;
        };
        const visit = (x: ts.Node) => {
          if (
            ts.isCallExpression(x) &&
            x.getStart(sf) >= from &&
            x.getEnd() <= body.getEnd()
          )
            bodySites.push(...calleeSites(x));
          else ts.forEachChild(x, visit);
        };
        visit(body);
        facts.loopBody = bodySites.map((d) => d.id);
      }
    }
    if (!elseRange) {
      facts.defaultKept = [];
      for (const id of thenIds) {
        const site = siteById.get(id);
        if (!site || site.category !== "state-write") continue;
        const deps = stateWriteDeps(site) ?? [];
        if (deps.length)
          facts.defaultKept.push({
            write: id,
            dependents: deps.map((d) => ({ site: d.site.id, label: d.label })),
          });
      }
    }
    return facts;
  }
  // Preserve the reference traversal order. The bounded recursive flow cache is
  // populated effect-first in the prototype; warming it in decision order can
  // change the recorded paths even when the syntax is identical.
  for (const s of effectSites) allBoundaries(s);
  for (const s of sites)
    if (s.kind === "decision") decisionFacts.set(s.id, analyzeDecision(s));

  const logEffectSites = effectSites.filter((e) => e.category === "log");
  /** Log sites an observation's message constraints admit; only the analyzer knows the template syntax. */
  const admittedLogSites = (ob: Observation): string[] | undefined => {
    if (!ob.pattern && !ob.literal && !ob.fragment) return undefined;
    return logEffectSites.filter((e) => messageFits(ob, e)).map((e) => e.id);
  };
  const suppressedObservations: {
    test: string;
    where: string;
    reason: string;
  }[] = [];
  type WitnessIssueKind =
    | "capture-unavailable"
    | "call-not-recorded"
    | "call-incomplete"
    | "mixed-call-outcomes"
    | "call-failed"
    | "uninstrumented-observation";
  function witnessIssue(
    test: string,
    ob: Observation,
  ): WitnessIssueKind | undefined {
    const reason = assertionWitnessIssue(
      runtimePhases.get(test),
      ob.assertionSource,
      ob.assertionMethod,
    );
    if (!reason) return undefined;
    suppressedObservations.push({
      test,
      where: ob.where,
      reason,
    });
    return reason;
  }
  const factObservation = (ob: Observation) => ({
    boundary: ob.boundary,
    facet: ob.facet,
    strength: ob.strength,
    where: ob.where,
    assertionSource: ob.assertionSource,
    assertionMethod: ob.assertionMethod,
    negative: ob.negative,
    callList: ob.callList,
    mock: ob.mock,
    comparison: ob.comparison,
    weak: ob.weak,
    implicit: ob.implicit,
    runtime: ob.runtime,
    logSites: admittedLogSites(ob),
    // a regex several log sites can satisfy pins none of them individually; a whole literal or a
    // fragment does not carry that ambiguity, so the rule is about patterns only
    patternShared: ob.pattern
      ? logEffectSites.filter((e) => patternMatchesSite(ob.pattern!, e))
          .length > 1
      : undefined,
  });
  const factTests = runtimeTests
    .map((rt) => {
      const st = staticFor(rt);
      if (!st) return undefined;
      const checked = st.observations.map((ob) => ({
        ob,
        kind: witnessIssue(rt.id, ob),
      }));
      const observations = checked
        .filter(({ kind }) => !kind)
        .map(({ ob }) => factObservation(ob));
      // Missing transport applies to the whole test, even when no operand could
      // be modeled. An empty phase file is different from no phase file.
      const witnessIssues = [
        ...(!runtimePhases.has(rt.id)
          ? [{ kind: "capture-unavailable" as const }]
          : []),
        ...checked
          .filter(({ kind }) => kind && kind !== "capture-unavailable")
          .map(({ ob, kind }) => ({
            kind: kind!,
            source: ob.assertionSource,
            operation: ob.assertionMethod,
            observation: factObservation(ob),
          })),
      ];
      return {
        id: rt.id,
        file: st.file,
        observations,
        ...(witnessIssues.length ? { witnessIssues } : {}),
        sinks: st.sinks,
        rendered: [...st.rendered],
      };
    })
    .filter((t) => t !== undefined);
  // vi.mock boundaries depend on the test file, not the test: one entry per (file, site) pair that has any
  const mocksByTestFile: Record<string, Record<string, Boundary[]>> = {};
  for (const file of new Set(factTests.map((t) => t.file))) {
    const perSite: Record<string, Boundary[]> = {};
    for (const s of sites) {
      const bs = moduleMockBoundaries(s, file);
      if (bs.length) perSite[s.id] = bs;
    }
    if (Object.keys(perSite).length) mocksByTestFile[file] = perSite;
  }
  const factSites = sites.map((s) => {
    const { bounds, reached } = allBoundaries(s);
    const tTrue = testsWithOutcome(s, true);
    const tFalse = testsWithOutcome(s, false);
    const selected = testsWhereSelected(s);
    const covered = runtimeTests
      .filter((t) => covers(t.id, s))
      .map((t) => t.id);
    const derive =
      s.kind === "effect"
        ? (deriveDeps(s) ?? []).map((d) => ({
            site: d.site.id,
            label: d.label,
            strength: d.strength,
            requiresTotal: d.requiresTotal,
          }))
        : [];
    return {
      id: s.id,
      file: s.file,
      line: s.start.line,
      kind: s.kind,
      category: s.category,
      classification: s.classification,
      owner: s.owner,
      method: s.method,
      bounds,
      // A site reached by another site's value is read at its own boundary only: the flow that carried
      // the value there does not carry it onward, so `bounds` (which includes that flow) is too wide.
      directBounds: directBoundaries(s),
      reached: reached.map((r) => r.id),
      coveredBy: covered,
      objectValuedReturn:
        (s.category === "return" || s.category === "callback-return") &&
        isObjectValuedReturn(s),
      unmodelledShapes: unmodelledOperands(
        s,
        runtimeTests.filter((t) => covers(t.id, s)),
      ),
      ...(s.kind === "decision"
        ? {
            decision: {
              ...(decisionFacts.get(s.id) ?? {}),
              outcomes:
                tTrue && tFalse
                  ? { true: [...tTrue], false: [...tFalse] }
                  : undefined,
              selected: selected ? [...selected] : undefined,
            },
          }
        : {}),
      ...(derive.length ? { derive } : {}),
    };
  });

  return {
    pragmas: pragmaCollector.finish(
      runtimeTests.flatMap((rt) => {
        const st = staticFor(rt);
        return st
          ? [
              {
                testKey: staticTestKey(st.file, st.line, st.name),
                id: rt.id,
                phases: runtimePhases.get(rt.id),
              },
            ]
          : [];
      }),
    ),
    facts: {
      schema: 1,
      root,
      sites: factSites,
      tests: factTests,
      mocksByTestFile,
    },
    diagnostics: {
      suppressedObservations,
      observationPolicy:
        "source-linked-v3: exact successful call witness; rejected witnesses retain typed provenance, not value credit",
      runtimeTests: runtimeTests.length,
      linkedTests: factTests.length,
      staticTests: staticTests.length,
      linkedByAssertionLines: linkedByPhases,
      linkedByTitle,
      linkWarnings,
      unrecognizedOperands: [...unrecognized].map(([shape, count]) => ({
        shape,
        count,
      })),
      compilerVersion: frontend.version,
      compilerFrontend: frontend.kind,
      compilerLimitations: [...frontend.limitations].sort(),
    },
  };
}
export type AnalysisResult = ReturnType<typeof analyze>;
export type Facts = AnalysisResult["facts"];
