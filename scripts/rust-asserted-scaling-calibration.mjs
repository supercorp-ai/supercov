// A frozen synthetic scaling workload, not a global assertion-accuracy oracle.
// No recognizer tuning is performed by this script. All generated data/logs are
// retained in fresh temporary directories, never in sample application repos.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { captureFixture, root } from "./rust-asserted-calibration-harness.mjs";

const digest = (data) => createHash("sha256").update(data).digest("hex");

export function workload(groups) {
  assert.ok(Number.isSafeInteger(groups) && groups > 0 && groups <= 128);
  const library = [];
  const imports = ["use asserted_scale_fixture::*;"];
  const tests = [];
  for (let i = 0; i < groups; i++) {
    const input = i + 7;
    const expected = input * 2 + i;
    library.push(
      `pub fn producer_${i}(value: i32) -> i32 { decoy_${i}(value); if value >= 0 && value < 10000 { value * 2 + ${i} } else { 0 } }`,
      `pub fn decoy_${i}(value: i32) -> i32 { value * 3 }`,
      `pub fn masked_${i}(value: i32) -> i32 { value * 2 }`,
      `pub fn never_${i}(value: i32) -> i32 { value + 1 }`,
    );
    imports.push(`use asserted_scale_fixture::producer_${i} as observed_${i};`);
    tests.push(
      `#[test]\nfn direct_${i}() { assert_eq!(${i % 2 ? `${expected}, observed_${i}(${input})` : `observed_${i}(${input}), ${expected}`}); }`,
      `#[test]\nfn copies_${i}() { let original = observed_${i}(${input}); let saved = original; let saved = saved; assert_eq!(saved, ${expected}); }`,
      `#[test]\nfn masked_${i}() { assert_eq!(asserted_scale_fixture::masked_${i}(${input}) % 2, 0); }`,
    );
  }
  const files = {
    "Cargo.toml":
      '[package]\nname = "asserted_scale_fixture"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n\n[lib]\ndoctest = false\n',
    "src/lib.rs": library.join("\n") + "\n",
    "tests/contract.rs":
      imports.join("\n") + "\n\n" + tests.join("\n\n") + "\n",
  };
  return {
    groups,
    files,
    hashes: Object.fromEntries(
      Object.entries(files).map(([name, data]) => [name, digest(data)]),
    ),
    sourceBytes: Object.values(files).reduce(
      (n, text) => n + Buffer.byteLength(text),
      0,
    ),
    expected: {
      functions: 4 * groups,
      tests: 3 * groups,
      uncalled: groups,
      witnesses: 2 * groups,
      witnessedBoundaries: groups,
    },
  };
}

export function semanticReport(report) {
  const {
    loadMs,
    analysisAndJoinMs,
    sourceReadVerifyAnalyzeMs,
    queryTimings,
    ...semantic
  } = report;
  return semantic;
}

export function canonicalRuntimeAnalysis(analysis) {
  // Cross-run phase IDs contain invocation nonces. Verify a bijection, never
  // drop phase membership. This fixture has one invocation per test/assertion.
  const byPhase = new Map();
  const byLabel = new Map();
  const executionLinks = analysis.executionLinks.map((link) => {
    for (const field of ["phase", "attempt", "operation", "assertionSource"])
      assert.ok(typeof link[field] === "string" && link[field].length > 0);
    const label = JSON.stringify([
      link.attempt,
      link.operation,
      link.assertionSource,
    ]);
    assert.ok(
      !byPhase.has(link.phase) || byPhase.get(link.phase) === label,
      "one phase claimed multiple test/assertion identities",
    );
    assert.ok(
      !byLabel.has(label) || byLabel.get(label) === link.phase,
      "multiple phases claimed one fixture assertion invocation",
    );
    byPhase.set(link.phase, label);
    byLabel.set(label, link.phase);
    return { ...link, phase: label };
  });
  return { ...analysis, executionLinks };
}

function verify(report, fixture, enabled) {
  const { expected, groups } = fixture;
  assert.equal(report.points.length, expected.functions);
  assert.equal(report.analysis.facts.sites.length, expected.functions);
  assert.equal(report.analysis.facts.tests.length, expected.tests);
  assert.equal(
    report.analysis.facts.sites.filter((s) => s.coveredBy.length === 0).length,
    expected.uncalled,
  );
  assert.equal(
    report.sourceAnalysis.witnesses.length,
    enabled ? expected.witnesses : 0,
  );
  assert.equal(
    report.sourceResolutions.filter((r) => r.status === "evident").length,
    enabled ? expected.witnessedBoundaries : 0,
  );
  assert.equal(
    report.sourceResolutions.filter((r) => r.status === "unresolved").length,
    enabled
      ? expected.functions - expected.witnessedBoundaries
      : expected.functions,
  );
  assert.equal(report.analysis.unsettledAssertions.length, 0);
  assert.equal(report.coverageInventory.unmeasured.length, 0);
  if (!enabled) {
    assert.equal(report.compilerAssertionIdentities, null);
    return;
  }
  for (let i = 0; i < groups; i++) {
    const point = report.points.find((p) =>
      p.source.startsWith(`pub fn producer_${i}(`),
    );
    const decoy = report.points.find((p) =>
      p.source.startsWith(`pub fn decoy_${i}(`),
    );
    const witnesses = report.sourceAnalysis.witnesses.filter(
      (w) => w.point === point.id,
    );
    assert.equal(witnesses.length, 2);
    assert.ok(witnesses.every((w) => w.compilerResolved));
    const direct = witnesses.find((w) => !w.viaLocal);
    const local = witnesses.find((w) => w.viaLocal);
    assert.equal(local.bindings.length, 3);
    assert.equal(local.expected, String((i + 7) * 2 + i));
    assert.ok(
      report.analysis.executionLinks.some(
        (l) => l.point === decoy.id && l.phase === direct.phase,
      ),
    );
    assert.ok(
      !report.sourceAnalysis.witnesses.some((w) => w.point === decoy.id),
    );
  }
}

export function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

function main() {
  assert.ok(
    process.argv.length === 2 ||
      (process.argv.length === 3 && process.argv[2] === "--plan-only"),
    "only --plan-only is supported; the experiment sizes/order are frozen",
  );
  assert.ok(
    !process.env.RUSTFLAGS && !process.env.CARGO_ENCODED_RUSTFLAGS,
    "start without custom Rust flags for this controlled benchmark",
  );
  const output = mkdtempSync(join(tmpdir(), "supercov-asserted-scaling-"));
  const binary = join(root, "target/release/supercov");
  const analyzer = join(root, "target/release/examples/rust_asserted_runtime");
  const companion = join(
    root,
    "spikes/rustc-backend/target/release/supercov-rustc-backend-spike",
  );
  const fixtures = [8, 32, 128].map(workload);
  const plan = {
    schema: 1,
    kind: "synthetic-scaling-not-accuracy",
    repetitions: 3,
    warmQueries: 3,
    binaries: Object.fromEntries(
      [binary, analyzer, companion].map((path) => [
        path,
        digest(readFileSync(path)),
      ]),
    ),
    fixtures,
  };
  writeFileSync(
    join(output, "plan.json"),
    JSON.stringify(plan, null, 2) + "\n",
  );
  console.log(`Frozen plan and results: ${output}`);
  if (process.argv.includes("--plan-only")) return;
  const captures = [];
  // Warm/rebuild the exact libtest bundle before timed pairs. captureFixture
  // separately reports setup, so it is not included in the run-phase comparison.
  for (const fixture of fixtures) {
    for (let repetition = 0; repetition < plan.repetitions; repetition++) {
      const pair = new Map();
      for (const enabled of repetition % 2 ? [true, false] : [false, true]) {
        console.log(
          `Families ${fixture.groups}, repetition ${repetition + 1}, identities ${enabled ? "on" : "off"}`,
        );
        const capture = captureFixture(
          "generated-scaling",
          (project) => {
            for (const [name, expectedHash] of Object.entries(fixture.hashes))
              assert.equal(
                digest(readFileSync(join(project, name))),
                expectedHash,
              );
          },
          {
            binary,
            analyzer,
            companion,
            separateNativeBuild: true,
            env: {
              RUSTFLAGS: enabled ? "--cfg supercov_assertion_identities" : "",
            },
            prepare(project) {
              for (const [name, contents] of Object.entries(fixture.files)) {
                mkdirSync(dirname(join(project, name)), { recursive: true });
                writeFileSync(join(project, name), contents);
              }
            },
          },
        );
        const {
          report,
          run,
          timings,
          runMetadata,
          project,
          archive,
          temporary,
        } = capture;
        verify(report, fixture, enabled);
        const warmQueries = [];
        for (let i = 0; i < plan.warmQueries; i++) {
          const next = JSON.parse(
            run(`query-warm-${i}`, analyzer, [
              archive,
              "src/lib.rs",
              "--source-root",
              project,
            ]).stdout,
          );
          assert.deepEqual(
            semanticReport(next),
            semanticReport(report),
            "query replay moved a verdict or evidence",
          );
          warmQueries.push({
            processMs: timings[`query-warm-${i}`],
            ...next.queryTimings,
          });
        }
        const row = {
          groups: fixture.groups,
          repetition,
          enabled,
          temporary,
          sourceHashes: fixture.hashes,
          sourceBytes: fixture.sourceBytes,
          identityRecords:
            report.compilerAssertionIdentities?.records.length ?? 0,
          sourceWitnesses: report.sourceAnalysis.witnesses.length,
          rawEvidence: runMetadata.rawEvidence,
          runPhases: {
            ...runMetadata.timings,
            preExecutionSetupBuildMs:
              runMetadata.timings.instrumentedBuildMs -
              runMetadata.timings.testCommandMs,
          },
          firstQuery: { processMs: timings.analyze, ...report.queryTimings },
          warmQueries,
          timings,
        };
        captures.push(row);
        pair.set(enabled, capture);
        writeFileSync(
          join(output, "captures.json"),
          JSON.stringify(captures, null, 2) + "\n",
        );
      }
      const off = pair.get(false),
        on = pair.get(true);
      assert.deepEqual(
        on.report.coverageInventory,
        off.report.coverageInventory,
        "identity mode changed the denominator",
      );
      assert.deepEqual(
        canonicalRuntimeAnalysis(on.report.analysis),
        canonicalRuntimeAnalysis(off.report.analysis),
        "identity mode changed runtime evidence",
      );
      assert.equal(
        on.runMetadata.integrity.fingerprint.source,
        off.runMetadata.integrity.fingerprint.source,
      );
      console.log(
        "Off/on inventory, runtime analysis and complete source hash match.",
      );
    }
  }
  const summaries = [];
  for (const fixture of fixtures) {
    for (const enabled of [false, true]) {
      const rows = captures.filter(
        (r) => r.groups === fixture.groups && r.enabled === enabled,
      );
      summaries.push({
        groups: fixture.groups,
        functions: fixture.expected.functions,
        tests: fixture.expected.tests,
        enabled,
        identityRecords: rows[0].identityRecords,
        witnesses: rows[0].sourceWitnesses,
        compressedBytes: median(rows.map((r) => r.rawEvidence.compressedBytes)),
        uncompressedBytes: median(
          rows.map((r) => r.rawEvidence.uncompressedBytes),
        ),
        medianPreExecutionSetupBuildMs: median(
          rows.map((r) => r.runPhases.preExecutionSetupBuildMs),
        ),
        medianTestCommandMs: median(rows.map((r) => r.runPhases.testCommandMs)),
        medianEvidencePublicationMs: median(
          rows.map((r) => r.runPhases.evidencePublicationMs),
        ),
        medianPrivateRunProcessMs: median(
          rows.map((r) => r.timings.instrumented),
        ),
        medianNativeBuildMs: median(rows.map((r) => r.timings["native-build"])),
        medianNativeCargoMs: median(
          rows.flatMap((r) =>
            [0, 1, 2].map((i) => r.timings[`native-warm-${i}`]),
          ),
        ),
        medianNativeBinaryMs: median(
          rows.flatMap((r) =>
            [0, 1, 2].map((i) => r.timings[`native-binary-${i}`]),
          ),
        ),
        medianWarmQuery: Object.fromEntries(
          Object.keys(rows[0].warmQueries[0]).map((key) => [
            key,
            median(rows.flatMap((r) => r.warmQueries.map((q) => q[key]))),
          ]),
        ),
      });
    }
  }
  const results = {
    schema: 1,
    output,
    allPairsMatched: true,
    summaries,
    claims: {
      globalAssertionScore: null,
      mutationSpeedup: null,
      runtimeOverheadBoundEstablished: false,
      instrumentedBuildTimerIncludesExecution: true,
      workload: "synthetic, held out from earlier recognizer development",
    },
  };
  writeFileSync(
    join(output, "results.json"),
    JSON.stringify(results, null, 2) + "\n",
  );
  console.log(JSON.stringify(results, null, 2));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1])
  main();
