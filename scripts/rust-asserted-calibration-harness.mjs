// Shared real-Cargo calibration harness. Artifacts stay in a fresh temp dir.
import assert from "node:assert/strict";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

export const root = resolve(import.meta.dirname, "..");

export function captureFixture(name, validate = () => {}, options = {}) {
  const temporary = mkdtempSync(join(tmpdir(), "supercov-rust-asserted-"));
  const project = join(temporary, "fixture");
  if (options.prepare) options.prepare(project);
  else
    cpSync(join(root, "crates/supercov-engine/tests/fixtures", name), project, {
      recursive: true,
      filter: (path) =>
        !path
          .split("/")
          .some((p) =>
            ["target", ".supercov", "mutants.out", "Cargo.lock"].includes(p),
          ),
    });
  console.log(`Calibration artifacts: ${temporary}`);
  validate(project);
  const binary = options.binary ?? join(root, "target/debug/supercov");
  const analyzer =
    options.analyzer ??
    join(root, "target/debug/examples/rust_asserted_runtime");
  const companion =
    options.companion ??
    join(
      root,
      "spikes/rustc-backend/target/debug/supercov-rustc-backend-spike",
    );
  const timings = {};
  function run(label, command, args, runOptions = {}) {
    const started = performance.now();
    const output = spawnSync(command, args, {
      cwd: project,
      encoding: "utf8",
      timeout: 600_000,
      maxBuffer: 32 * 1024 * 1024,
      env: { ...process.env, RUSTUP_TOOLCHAIN: "1.95.0", ...options.env },
      ...runOptions,
    });
    timings[label] = performance.now() - started;
    writeFileSync(join(temporary, `${label}.stdout`), output.stdout ?? "");
    writeFileSync(join(temporary, `${label}.stderr`), output.stderr ?? "");
    console.log(
      `${label}: ${timings[label].toFixed(1)} ms, exit ${output.status}`,
    );
    assert.ok(
      [0, ...(runOptions.allowedStatuses ?? [])].includes(output.status),
      `${label}: ${output.error ?? ""}\n${output.stdout}\n${output.stderr}`,
    );
    return output;
  }
  if (options.separateNativeBuild)
    run("native-build", "cargo", ["test", "--offline", "--no-run"]);
  run("native-cold", "cargo", ["test", "--offline"]);
  for (let i = 0; i < 3; i++)
    run(`native-warm-${i}`, "cargo", ["test", "--offline"]);
  const testBinaries = readdirSync(join(project, "target/debug/deps")).filter(
    (name) => /^contract-[a-f0-9]+$/.test(name),
  );
  assert.equal(testBinaries.length, 1);
  for (let i = 0; i < 3; i++) {
    run(
      `native-binary-${i}`,
      join(project, "target/debug/deps", testBinaries[0]),
      [],
    );
  }
  const rustc = run("locate-rustc", "rustup", ["which", "rustc"]).stdout.trim();
  const sysroot = run("locate-sysroot", rustc, [
    "--print",
    "sysroot",
  ]).stdout.trim();
  mkdirSync(join(temporary, "libtest-build"));
  run("prepare-libtest", binary, [
    "__build-rust-libtest-companion",
    join(sysroot, "lib/rustlib/src/rust/library/test"),
    join(temporary, "libtest-build"),
    rustc,
    companion,
  ]);
  // Public Rust in this checkout is still the legacy source rewriter.
  const instrumented = run("instrumented", binary, ["__run-rust-compiler"], {
    input: JSON.stringify({
      root: project,
      command: ["cargo", "test", "--offline"],
      runId: "run_a123456789abcdef",
      startedAt: new Date().toISOString(),
      wrapperPath: binary,
      companionCandidates: [companion],
      requirePublicCapabilities: false,
    }),
  });
  const compilerRun = JSON.parse(instrumented.stdout);
  assert.equal(compilerRun.exitCode, 0);
  const archive = join(compilerRun.runDirectory, "evidence.raw.gz");
  const runMetadata = JSON.parse(
    readFileSync(join(compilerRun.runDirectory, "run.json"), "utf8"),
  );
  // Analyze before mutants.out is created: it changes the current configuration
  // fingerprint. No source/config mismatch is silently bypassed.
  const report = JSON.parse(
    run("analyze", analyzer, [archive, "src/lib.rs", "--source-root", project])
      .stdout,
  );
  writeFileSync(
    join(temporary, "analysis.json"),
    JSON.stringify(report, null, 2),
  );
  return {
    temporary,
    project,
    archive,
    report,
    run,
    timings,
    runMetadata,
    analyzer,
  };
}
