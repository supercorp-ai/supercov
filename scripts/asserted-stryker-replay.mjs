/** Frozen historical Stryker replay, not mutation execution or a product feature.
 * All modified analyzer variants exist only as in-memory diagnostic modules.
 * Source/tests and the original prototype stay read-only. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { resolve, relative } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import {
  metrics,
  availabilityPrediction,
} from "./asserted-stryker-metrics.mjs";

const repository = resolve(import.meta.dirname, "..");
const ts = createRequire(
  resolve(repository, "analyzers/typescript/package.json"),
)("typescript");
const opts = {};
for (let i = 2; i < process.argv.length; i += 2) {
  assert.ok(
    [
      "--prototype",
      "--previous-analyzer",
      "--second-project",
      "--output",
    ].includes(process.argv[i]),
  );
  assert.ok(process.argv[i + 1]);
  assert.equal(opts[process.argv[i]], undefined);
  opts[process.argv[i]] = resolve(process.argv[i + 1]);
}
for (const key of [
  "--prototype",
  "--previous-analyzer",
  "--second-project",
  "--output",
])
  assert.ok(opts[key], `required ${key}`);
assert.ok(
  !existsSync(opts["--output"]),
  "Refusing to overwrite a prior replay",
);
const prototype = opts["--prototype"],
  previous = opts["--previous-analyzer"];
const prototypeTools = resolve(prototype, "tools/asserted-coverage");
const binary = resolve(repository, "target/debug/examples/asserted_join");
assert.ok(
  existsSync(binary),
  "Build: cargo build -p supercov-engine --example asserted_join",
);
const read = (path) => JSON.parse(readFileSync(path, "utf8"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const run = (cmd, args, options = {}) => {
  const result = spawnSync(cmd, args, {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: 300000,
    ...options,
  });
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  return result.stdout;
};
function treeHash(root, directories) {
  const files = [];
  function walk(path) {
    for (const entry of readdirSync(path, { withFileTypes: true }).sort(
      (a, b) => a.name.localeCompare(b.name),
    )) {
      if (["node_modules", ".git", ".supercov"].includes(entry.name)) continue;
      const next = resolve(path, entry.name);
      if (entry.isDirectory()) walk(next);
      else if (entry.isFile()) files.push(next);
    }
  }
  for (const dir of directories) walk(resolve(root, dir));
  const h = createHash("sha256");
  for (const file of files)
    h.update(relative(root, file))
      .update("\0")
      .update(readFileSync(file))
      .update("\0");
  return { files: files.length, sha256: h.digest("hex") };
}
const compileModule = async (source) => {
  const compiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  return import(
    "data:text/javascript;base64," + Buffer.from(compiled).toString("base64")
  );
};
const prototypeSource = readFileSync(
  resolve(prototypeTools, "resolve-oracles.ts"),
  "utf8",
);
const ast = ts.createSourceFile(
  "prototype.ts",
  prototypeSource,
  ts.ScriptTarget.Latest,
  true,
);
const functionSource = (name) => {
  const matches = ast.statements.filter(
    (n) => ts.isFunctionDeclaration(n) && n.name?.text === name,
  );
  assert.equal(matches.length, 1, name);
  return matches[0].getText(ast);
};
const variableSource = (name) => {
  const matches = ast.statements
    .filter(ts.isVariableStatement)
    .flatMap((n) => [...n.declarationList.declarations])
    .filter((n) => n.name.getText(ast) === name);
  assert.equal(matches.length, 1, name);
  return `const ${matches[0].getText(ast)};`;
};
// Prediction functions are the prototype's own source, not a rewritten join or
// an approximation of its mutation rules. Only the variable bindings differ.
const { predictor } =
  await compileModule(`export function predictor(sites, rows) {
  const resolutions = new Map(rows.map(r => [r.site, r]));
  ${variableSource("RANK")}
  ${variableSource("REMOVAL")}
  ${functionSource("predict")}
  ${functionSource("predictMutant")}
  return predictMutant;
}`);
const removal = (m) =>
  ["BlockStatement", "ArrowFunction", "MethodExpression"].includes(
    m.mutatorName,
  ) ||
  (m.mutatorName === "CallExpression" && m.replacement.trim() === ";");
function replaceOnce(source, needle, replacement) {
  assert.equal(
    source.split(needle).length,
    2,
    `diagnostic patch anchor changed: ${needle}`,
  );
  return source.replace(needle, replacement);
}
async function analyzer(path, transform = (source) => source) {
  const source = readFileSync(path, "utf8");
  let modified = transform(source);
  modified = replaceOnce(
    modified,
    'from "./compiler.js"',
    `from ${JSON.stringify(pathToFileURL(resolve(repository, "analyzers/typescript/dist/compiler.js")).href)}`,
  );
  const { analyze } = await compileModule(modified);
  return {
    analyze,
    sourceSha256: hash(source),
    diagnosticModuleSha256: hash(modified),
  };
}
const oldPath = resolve(previous, "src/analyze.ts");
const currentPath = resolve(repository, "analyzers/typescript/src/analyze.ts");
const old = await analyzer(oldPath);
const noHelpers = await analyzer(oldPath, (source) =>
  replaceOnce(source, "if (HELPERS.has(callee.text)) {", "if (false) {"),
);
const current = await analyzer(currentPath);
const noWitnessGate = await analyzer(currentPath, (source) => {
  const tree = ts.createSourceFile(
    "current.ts",
    source,
    ts.ScriptTarget.Latest,
    true,
  );
  let body;
  function visit(n) {
    if (ts.isFunctionDeclaration(n) && n.name?.text === "witnessIssue") {
      assert.equal(body, undefined);
      body = n.body;
    }
    ts.forEachChild(n, visit);
  }
  visit(tree);
  assert.ok(body);
  return (
    source.slice(0, body.getStart(tree) + 1) +
    "\nreturn undefined; // COUNTERFACTUAL ONLY\n" +
    source.slice(body.getStart(tree) + 1)
  );
});
const work = mkdtempSync(resolve(tmpdir(), "supercov-stryker-replay-"));
const report = {
  schema: 1,
  work,
  node: process.version,
  compiler: ts.version,
  purpose:
    "Frozen historical replay; no mutation or application tests executed; not a fresh semantic-accuracy benchmark",
  caveats: [
    "Stripped Stryker fixtures do not contain source text, so their original source/test hashes cannot be reconstructed from them.",
    "The broad Essential SEO Stryker fixture predates subsequently added tests. This comparison holds that same historical oracle constant for both analyzers.",
    "Timeout counts as killed in the original benchmark. Timeout-excluded metrics are also reported.",
    "Diagnostic ablations deliberately remove safeguards. They are not deployable fixes or certified assertions.",
  ],
  provenance: {
    prototypeCommit: run("git", ["rev-parse", "HEAD"], {
      cwd: prototype,
    }).trim(),
    prototypeSourceSha256: hash(prototypeSource),
    currentAnalyzer: current.sourceSha256,
    previousAnalyzer: old.sourceSha256,
    joinBinarySha256: hash(readFileSync(binary)),
  },
  samples: {},
};

for (const name of ["supergateway", "essentialSeo"]) {
  const seo = name === "essentialSeo";
  const projectRoot = seo ? opts["--second-project"] : prototype;
  const sourceDirs = seo ? ["app", "tests/unit"] : ["src", "tests"];
  const sourceBefore = treeHash(projectRoot, sourceDirs);
  const originalInput = resolve(
    prototypeTools,
    seo ? "out-essential-seo-trial" : "out",
  );
  const inputDirectory = resolve(work, name);
  mkdirSync(inputDirectory);
  cpSync(resolve(originalInput, "cov"), resolve(inputDirectory, "cov"), {
    recursive: true,
  });
  cpSync(
    resolve(originalInput, "inventory.json"),
    resolve(inputDirectory, "inventory.json"),
  );
  const fixturePath = resolve(
    prototypeTools,
    "fixtures",
    seo
      ? "stryker-essential-seo-all-2026-09-09.json"
      : "stryker-src-lib-step3-2026-09-09.json",
  );
  const fixture = read(fixturePath),
    inventory = read(resolve(inputDirectory, "inventory.json"));
  const env = {
    ...process.env,
    OUT_DIR: inputDirectory,
    STRYKER_REPORT: fixturePath,
  };
  for (const k of [
    "SRC_DIR",
    "TEST_DIR",
    "TSCONFIG",
    "COVERAGE_RUNNER",
    "RUNTIME_OBS",
    "FACTS",
    "TRIAGE",
    "PROBES",
    "BASELINE",
    "DEBUG_SITE",
  ])
    delete env[k];
  const extra = seo
    ? {
        sourceDir: "app",
        testDir: "tests/unit",
        tsconfig: "tsconfig.json",
        coverageRunner: "vitest",
      }
    : {};
  if (seo)
    Object.assign(env, {
      SRC_DIR: "app",
      TEST_DIR: "tests/unit",
      TSCONFIG: "tsconfig.json",
      COVERAGE_RUNNER: "vitest",
    });
  const stdout = run(
    process.execPath,
    [
      resolve(prototype, "node_modules/tsx/dist/cli.mjs"),
      resolve(prototypeTools, "resolve-oracles.ts"),
      projectRoot,
    ],
    { cwd: prototype, env },
  );
  const reference = read(
    resolve(inputDirectory, "resolution.json"),
  ).resolutions;
  const referenceFacts = read(resolve(inputDirectory, "facts.json"));
  const source = new Map(
    [
      ...new Set(
        inventory.sites.map((s) => s.file).concat(Object.keys(fixture.files)),
      ),
    ].map((file) => [file, readFileSync(resolve(projectRoot, file), "utf8")]),
  );
  for (const site of inventory.sites)
    assert.equal(
      source
        .get(site.file)
        .slice(site.pos, site.endPos)
        .replace(/\s+/g, " ")
        .slice(0, 100),
      site.text,
      `stale inventory ${site.id}`,
    );
  const offset = (file, p) => {
    const lines = [0];
    for (let i = 0; i < source.get(file).length; i++)
      if (source.get(file)[i] === "\n") lines.push(i + 1);
    assert.ok(p.line >= 1 && p.line <= lines.length && p.column >= 1);
    return lines[p.line - 1] + p.column - 1;
  };
  const referencePredict = predictor(inventory.sites, reference);
  const mutants = [];
  const excludedByStatus = {};
  for (const [file, data] of Object.entries(fixture.files))
    for (const m of data.mutants) {
      if (!["Killed", "Survived", "Timeout"].includes(m.status)) {
        excludedByStatus[m.status] = (excludedByStatus[m.status] ?? 0) + 1;
        continue;
      }
      const start = offset(file, m.location.start),
        end = offset(file, m.location.end);
      const cands = inventory.sites
        .filter((s) => s.file === file && s.pos < end && start < s.endPos)
        .sort((a, b) => a.endPos - a.pos - (b.endPos - b.pos));
      const containing = cands.filter((c) => c.pos <= start && c.endPos >= end);
      const site =
        containing.find((c) => c.classification !== "review") ??
        cands.find((c) => c.classification !== "review") ??
        containing[0] ??
        cands[0];
      const inside = removal(m)
        ? inventory.sites.filter(
            (s) => s.file === file && s.pos >= start && s.endPos <= end,
          )
        : [];
      const binary = referencePredict(
        file,
        start,
        end,
        site,
        m.mutatorName,
        m.replacement,
      );
      mutants.push({
        id: `${file}#${m.id}`,
        file,
        line: m.location.start.line,
        mutator: m.mutatorName,
        replacement: m.replacement,
        status: m.status,
        actualKilled: m.status !== "Survived",
        start,
        end,
        site,
        inside,
        referenceBinary: binary,
      });
    }
  const cohort = mutants.filter((m) => m.referenceBinary !== undefined);
  const baselineMetrics = metrics(cohort, "referenceBinary");
  assert.equal(cohort.length, seo ? 827 : 100);
  assert.equal(baselineMetrics.correct, seo ? 714 : 94);
  assert.ok(
    stdout.includes(`Agreement ${baselineMetrics.correct} of ${cohort.length}`),
  );
  assert.ok(
    stdout.includes(
      `| predicted killed | ${baselineMetrics.tp} | ${baselineMetrics.fp} |`,
    ),
    "the original report must independently confirm both positive cells",
  );
  assert.ok(
    stdout.includes(
      `| predicted survived | ${baselineMetrics.fn} | ${baselineMetrics.tn} |`,
    ),
    "the original report must independently confirm both negative cells",
  );
  const sample = {
    projectRoot,
    projectCommit: run("git", ["rev-parse", "HEAD"], {
      cwd: projectRoot,
    }).trim(),
    sourceAndTestFiles: sourceBefore,
    fixtureSha256: hash(readFileSync(fixturePath)),
    inventorySha256: hash(
      readFileSync(resolve(inputDirectory, "inventory.json")),
    ),
    capture: treeHash(inputDirectory, ["cov"]),
    phaseFiles: readdirSync(resolve(inputDirectory, "cov")).filter((n) =>
      n.endsWith(".phases.json"),
    ).length,
    sites: inventory.sites.length,
    linkedTests: referenceFacts.tests.length,
    executedMutants: mutants.length,
    fixedCohort: cohort.length,
    excludedByStatus,
    outsideCohort: mutants
      .filter((m) => m.referenceBinary === undefined)
      .map((m) => ({ id: m.id, status: m.status, site: m.site?.id ?? null })),
    alwaysPredictKilled: metrics(
      cohort.map((m) => ({ ...m, p: true })),
      "p",
    ),
    variants: {},
    transitions: [],
    rows: [],
  };
  const versions = [
    {
      name: "original",
      result: { resolutions: reference },
      facts: referenceFacts,
    },
    { name: "extractedOriginal", module: old },
    {
      name: "originalWithoutRuntimeInference",
      module: old,
      options: { runtimeObservations: false },
    },
    {
      name: "originalWithoutRuntimeOrHelperNames",
      module: noHelpers,
      options: { runtimeObservations: false },
    },
    {
      name: "currentWithoutWitnessGate_DIAGNOSTIC_ONLY",
      module: noWitnessGate,
    },
    { name: "current", module: current },
  ];
  const predictions = {};
  for (const version of versions) {
    const started = performance.now();
    let facts = version.facts,
      result = version.result;
    if (!facts)
      facts = version.module.analyze({
        projectRoot,
        inputDirectory,
        ...extra,
        ...version.options,
      }).facts;
    if (version.name.includes("DIAGNOSTIC_ONLY"))
      for (const test of facts.tests) delete test.witnessIssues;
    assert.deepEqual(
      facts.sites.map((s) => s.id),
      referenceFacts.sites.map((s) => s.id),
    );
    assert.deepEqual(
      facts.tests.map((t) => t.id),
      referenceFacts.tests.map((t) => t.id),
    );
    if (!result)
      result = JSON.parse(run(binary, [], { input: JSON.stringify(facts) }));
    const elapsedMs = performance.now() - started;
    const byId = new Map(result.resolutions.map((r) => [r.site, r]));
    const predict = predictor(inventory.sites, result.resolutions);
    const rows = cohort.map((m) => {
      const binary = predict(
        m.file,
        m.start,
        m.end,
        m.site,
        m.mutator,
        m.replacement,
      );
      const relevant = [m.site, ...m.inside]
        .filter(Boolean)
        .map((s) => byId.get(s.id));
      return {
        id: m.id,
        actualKilled: m.actualKilled,
        status: m.status,
        binary,
        availability: availabilityPrediction(binary, relevant),
        siteStatus: byId.get(m.site?.id)?.status,
        reason: byId.get(m.site?.id)?.reason?.kind,
        issueKinds: [
          ...new Set(
            relevant.flatMap((r) =>
              (r?.witnessIssues ?? []).map((w) => w.kind),
            ),
          ),
        ],
      };
    });
    if (version.name === "extractedOriginal")
      assert.deepEqual(
        rows.map((r) => r.binary),
        cohort.map((m) => m.referenceBinary),
        "original extraction must reproduce every mutant prediction",
      );
    predictions[version.name] = rows;
    sample.variants[version.name] = {
      elapsedMs,
      observations: facts.tests.reduce((n, t) => n + t.observations.length, 0),
      summary: result.summary ?? null,
      binary: metrics(rows, "binary"),
      availability: metrics(rows, "availability"),
      binaryExcludingTimeouts: metrics(
        rows.filter((r) => r.status !== "Timeout"),
        "binary",
      ),
      byFile: Object.fromEntries(
        [...new Set(cohort.map((m) => m.file))].map((file) => [
          file,
          metrics(
            rows.filter((r, i) => cohort[i].file === file),
            "binary",
          ),
        ]),
      ),
      diagnosticModuleSha256: version.module?.diagnosticModuleSha256 ?? null,
    };
    writeFileSync(
      resolve(inputDirectory, `${version.name}.resolutions.json`),
      JSON.stringify(result),
    );
    console.log(
      JSON.stringify({
        sample: name,
        variant: version.name,
        elapsedMs,
        observations: sample.variants[version.name].observations,
        binary: sample.variants[version.name].binary,
        availability: sample.variants[version.name].availability,
      }),
    );
  }
  const chain = [
    "original",
    "originalWithoutRuntimeInference",
    "originalWithoutRuntimeOrHelperNames",
    "currentWithoutWitnessGate_DIAGNOSTIC_ONLY",
    "current",
  ];
  for (let i = 1; i < chain.length; i++) {
    const before = predictions[chain[i - 1]],
      after = predictions[chain[i]];
    const changed = after
      .map((r, j) => ({ r, before: before[j] }))
      .filter(({ r, before }) => r.binary !== before.binary);
    sample.transitions.push({
      from: chain[i - 1],
      to: chain[i],
      changed: changed.length,
      lostTrueKills: changed.filter(
        ({ r, before }) =>
          before.binary === true && r.binary !== true && r.actualKilled,
      ).length,
      removedFalseKills: changed.filter(
        ({ r, before }) =>
          before.binary === true && r.binary !== true && !r.actualKilled,
      ).length,
      gainedTrueKills: changed.filter(
        ({ r, before }) =>
          before.binary !== true && r.binary === true && r.actualKilled,
      ).length,
      addedFalseKills: changed.filter(
        ({ r, before }) =>
          before.binary !== true && r.binary === true && !r.actualKilled,
      ).length,
    });
  }
  sample.rows = cohort.map((m, i) => ({
    id: m.id,
    file: m.file,
    line: m.line,
    mutator: m.mutator,
    replacement: m.replacement,
    status: m.status,
    site: m.site?.id ?? null,
    owner: m.site?.owner ?? null,
    category: m.site?.category ?? null,
    predictions: Object.fromEntries(
      Object.entries(predictions).map(([name, rows]) => [
        name,
        {
          ...rows[i],
          binary: rows[i].binary ?? null,
          availability: rows[i].availability ?? null,
        },
      ]),
    ),
  }));
  assert.deepEqual(
    treeHash(projectRoot, sourceDirs),
    sourceBefore,
    "sample source/tests changed during analysis",
  );
  assert.deepEqual(
    treeHash(inputDirectory, ["cov"]),
    sample.capture,
    "capture changed during replay",
  );
  report.samples[name] = sample;
}
writeFileSync(opts["--output"], JSON.stringify(report, null, 2), {
  flag: "wx",
});
console.log(JSON.stringify({ output: opts["--output"], work }));
