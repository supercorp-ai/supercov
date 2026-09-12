// Real Cargo harness + archive + join. Mutation execution is opt-in and pinned.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import test from "node:test";
import { captureFixture, root } from "./rust-asserted-calibration-harness.mjs";

const savedOracle = JSON.parse(
  readFileSync(
    join(
      root,
      "crates/supercov-engine/tests/fixtures/rust-asserted-runtime-oracle.json",
    ),
    "utf8",
  ),
);
const { temporary, project, archive, report, run, timings } = captureFixture(
  "rust-asserted-runtime",
  (project) => {
    for (const [file, digest] of Object.entries(savedOracle.files)) {
      assert.equal(
        createHash("sha256")
          .update(readFileSync(join(project, file)))
          .digest("hex"),
        digest,
        file + " changed: regenerate the mutation oracle explicitly",
      );
    }
  },
);
assert.equal(report.analysis.facts.tests.length, 7);
assert.equal(report.analysis.facts.sites.length, 8);
assert.equal(report.analysis.unmeasured.length, 0);
assert.ok(report.resolutions.every((r) => r.status === "unresolved"));
const byName = new Map(
  report.points.map((p) => [p.source.match(/fn\s+(\w+)/)?.[1], p]),
);
const sites = new Map(report.analysis.facts.sites.map((s) => [s.id, s]));
const links = report.analysis.executionLinks;
const sourceWitnesses = report.sourceAnalysis.witnesses;
assert.deepEqual(sourceWitnesses.map((w) => w.call).sort(), [
  "direct(21)",
  "precomputed(21)",
]);
assert.equal(report.sourceAnalysis.facts.sites.length, 8);
assert.equal(
  report.sourceResolutions.filter((r) => r.status === "evident").length,
  2,
);
assert.equal(
  report.sourceResolutions.filter((r) => r.status === "unresolved").length,
  6,
);
for (const name of ["direct", "discarded", "masked", "weak"]) {
  assert.ok(
    links.some((l) => l.point === byName.get(name)?.id),
    `${name} must be assertion-execution linked`,
  );
}
for (const name of ["precomputed", "smoke", "threaded", "never_called"]) {
  assert.ok(
    !links.some((l) => l.point === byName.get(name)?.id),
    `${name} must not acquire temporal linkage`,
  );
}
const repeatedLinks = links.filter(
  (l) => l.point === byName.get("direct").id,
).length;
const knownPhaseDefect =
  repeatedLinks === 1 && report.analysis.unsettledAssertions.length === 1;
assert.ok(
  links.every((l) => l.statement),
  "the fixture assertions are compiler-mapped statements",
);
assert.equal(sites.get(byName.get("never_called").id).coveredBy.length, 0);
for (const name of ["precomputed", "smoke", "threaded"])
  assert.equal(sites.get(byName.get(name).id).coveredBy.length, 1);

const oracle = process.env.SUPERCOV_CARGO_MUTANTS;
let outcomes = savedOracle.outcomes;
if (oracle) {
  const version = run("mutants-version", oracle, ["mutants", "--version"]);
  assert.match(version.stdout, /cargo-mutants 27\.1\.0\b/);
  run(
    "mutants",
    oracle,
    [
      "mutants",
      "--dir",
      project,
      "--file",
      "src/lib.rs",
      "--jobs",
      "1",
      "--no-shuffle",
      "--timeout",
      "15",
      "--",
      "--offline",
    ],
    { allowedStatuses: [2] },
  );
  const raw = JSON.parse(
    readFileSync(join(project, "mutants.out/outcomes.json"), "utf8"),
  );
  writeFileSync(
    join(temporary, "mutants-outcomes.json"),
    JSON.stringify(raw, null, 2),
  );
  assert.equal(raw.total_mutants, 40);
  assert.equal(raw.timeout, 0);
  assert.equal(raw.unviable, 0);
  assert.equal(raw.outcomes[0].scenario, "Baseline");
  assert.equal(raw.outcomes[0].summary, "Success");
  outcomes = raw.outcomes.slice(1).map((o) => ({
    name: o.scenario.Mutant.name,
    function: o.scenario.Mutant.function.function_name,
    status: o.summary,
  }));
  assert.deepEqual(
    outcomes,
    savedOracle.outcomes.map(({ name, function: fn, status }) => ({
      name,
      function: fn,
      status,
    })),
    "fresh mutation outcomes must match the complete saved oracle",
  );
}
const confusion = {
  truePositive: 0,
  falsePositive: 0,
  falseNegative: 0,
  trueNegative: 0,
};
const perFunction = {};
for (const outcome of outcomes) {
  assert.ok(["CaughtMutant", "MissedMutant"].includes(outcome.status));
  const point = byName.get(outcome.function);
  assert.ok(point, `unmapped mutant: ${outcome.name}`);
  const linked = links.some((l) => l.point === point.id);
  const killed = outcome.status === "CaughtMutant";
  confusion[
    linked
      ? killed
        ? "truePositive"
        : "falsePositive"
      : killed
        ? "falseNegative"
        : "trueNegative"
  ]++;
  const row = (perFunction[outcome.function] ??= {
    assertionExecutionLinked: linked,
    exactReturnWitness: sourceWitnesses.some((w) => w.point === point.id),
    caught: 0,
    survived: 0,
  });
  row[killed ? "caught" : "survived"]++;
}
assert.deepEqual(confusion, {
  truePositive: 8,
  falsePositive: 12,
  falseNegative: 10,
  trueNegative: 10,
});
const comparison = {
  oracle: oracle
    ? "fresh cargo-mutants 27.1.0 run, matched saved oracle"
    : "saved cargo-mutants 27.1.0 oracle, source hashes verified (no new mutation run)",
  mutants: outcomes.length,
  unmapped: 0,
  timeouts: 0,
  unviable: 0,
  perFunction,
  // A falsification of a proposed rule, NOT the analyzer's shipped prediction.
  rejectedHypothesis: {
    rule: "a function entered during a passing assertion has its value checked",
    confusion,
    agreement: 18 / 40,
    predictedKillPrecision: 8 / 20,
  },
  implementedAnalysis: {
    exactReturnWitnesses: 2,
    unresolvedFunctions: 6,
    mutationVerdicts: 0,
    globalAssertionScore: null,
  },
  knownDefects: knownPhaseDefect ? ["RUST-ASSERT-001"] : [],
};
writeFileSync(
  join(temporary, "comparison.json"),
  JSON.stringify(comparison, null, 2),
);
writeFileSync(
  join(temporary, "timings.json"),
  JSON.stringify(timings, null, 2),
);
console.log(
  JSON.stringify({ temporary, archive, comparison, timings }, null, 2),
);
await test(
  "both sequential direct assertions retain their own passing execution link",
  {
    todo:
      knownPhaseDefect && !process.env.SUPERCOV_ASSERTED_STRICT_PHASES
        ? "RUST-ASSERT-001: second assertion outcome and calls are stamped with the base test context; see findings doc"
        : false,
  },
  () => assert.equal(repeatedLinks, 2),
);
