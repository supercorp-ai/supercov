// The Rust validation benchmark (`.github/workflows/mutation-rust.yml`).
//
//   node benchmarks/mutation/rust/summarize.mjs plan
//     The workflow's matrices from corpus.json, as `truth=` and `supercov=`
//     lines for $GITHUB_OUTPUT (CRATES narrows them to some crates).
//
//   node benchmarks/mutation/rust/summarize.mjs report ARTIFACTS OUT
//     For each crate: Supercov's shard runs merged (`supercov merge`, in a
//     fresh clone at the tag), joined with cargo-mutants' outcomes by
//     scripts/cargo-mutants-agreement.mjs, and both tools' wall and CPU
//     time summed over their jobs; a merge that holds other than the
//     shards' verdicts added up is a violation. Writes OUT/<crate>.json, OUT/results.json
//     and a Markdown table on standard output. Exits 1 on a soundness
//     violation.

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

const here = import.meta.dirname;
const repository = path.resolve(here, "../../..");
const corpus = JSON.parse(readFileSync(path.join(here, "corpus.json"), "utf8"));
const wanted = (process.env.CRATES ?? "").split(/\s+/u).filter(Boolean);
const crates = corpus.crates.filter((crate) => wanted.length === 0 || wanted.includes(crate.name));

const [mode, artifacts, out] = process.argv.slice(2);

if (mode === "plan") {
  const truth = crates.flatMap((crate) =>
    Array.from({ length: crate.truthShards }, (_, index) => ({
      crate: crate.name,
      repo: crate.repo,
      tag: crate.tag,
      args: crate.cargoMutantsArgs ?? "",
      index,
      shard: index + 1,
      shards: crate.truthShards,
    })),
  );
  const supercov = crates.flatMap((crate) =>
    Array.from({ length: crate.supercovShards }, (_, index) => ({
      crate: crate.name,
      repo: crate.repo,
      tag: crate.tag,
      test: crate.test,
      shard: index + 1,
      shards: crate.supercovShards,
    })),
  );
  console.log(`truth=${JSON.stringify(truth)}`);
  console.log(`supercov=${JSON.stringify(supercov)}`);
} else if (mode === "report") {
  report(path.resolve(artifacts), path.resolve(out));
} else {
  console.error("usage: summarize.mjs plan | report ARTIFACTS OUT");
  process.exit(2);
}

/** Wall and CPU seconds from a GNU `time -v` file. */
function times(file) {
  if (!existsSync(file)) return null;
  const text = readFileSync(file, "utf8");
  const number = (label) => Number(text.match(new RegExp(`${label}: ([\\d.]+)`, "u"))?.[1] ?? 0);
  const clock = text.match(/Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): ([\d:.]+)/u)?.[1] ?? "0";
  const wall = clock.split(":").reduce((sum, part) => sum * 60 + Number(part), 0);
  return { wall, cpu: number("User time \\(seconds\\)") + number("System time \\(seconds\\)") };
}

function sum(entries) {
  const known = entries.filter(Boolean);
  return {
    jobs: entries.length,
    measured: known.length,
    wall: known.reduce((total, entry) => total + entry.wall, 0),
    cpu: known.reduce((total, entry) => total + entry.cpu, 0),
  };
}

function report(artifactsDir, outDir) {
  mkdirSync(outDir, { recursive: true });
  const binary = path.join(artifactsDir, "supercov-bin/supercov");
  const results = [];
  let violations = 0;
  for (const crate of crates) {
    const work = path.join(outDir, "work", crate.name);
    rmSync(work, { recursive: true, force: true });
    execFileSync("git", ["clone", "-q", "--depth", "1", "--branch", crate.tag, crate.repo, work]);
    const runs = path.join(work, ".supercov/runs");
    mkdirSync(runs, { recursive: true });
    const shards = Array.from({ length: crate.supercovShards }, (_, index) =>
      path.join(artifactsDir, `supercov-${crate.name}-${index + 1}`),
    );
    for (const shard of shards) {
      if (existsSync(path.join(shard, "runs"))) cpSync(path.join(shard, "runs"), runs, { recursive: true });
    }
    let merged = null;
    let note = "";
    try {
      if (crate.supercovShards > 1) {
        execFileSync(binary, ["merge"], { cwd: work, stdio: ["ignore", "ignore", "pipe"] });
      }
      merged = JSON.parse(execFileSync(binary, ["runs", "--json"], { cwd: work, encoding: "utf8" })).data.runs[0].id;
    } catch (error) {
      note = `merge failed: ${String(error.stderr ?? error.message).trim().split("\n").at(-1)}`;
    }
    const outcomes = readdirSync(artifactsDir)
      .filter((name) => name.startsWith(`truth-${crate.name}-`))
      .map((name) => path.join(artifactsDir, name, "mutants.out/outcomes.json"))
      .filter((file) => existsSync(file));
    let agreement = null;
    if (merged && outcomes.length > 0) {
      const args = [
        path.join(repository, "scripts/cargo-mutants-agreement.mjs"),
        "--root",
        work,
        "--run",
        merged,
        "--json",
        ...outcomes.flatMap((file) => ["--outcomes", file]),
      ];
      const weak = path.join(shards[0], "weak.jsonl");
      if (crate.supercovShards === 1 && existsSync(weak)) args.push("--weak", weak);
      try {
        agreement = JSON.parse(execFileSync("node", args, { encoding: "utf8" }));
      } catch (error) {
        // Exit 1 on a soundness violation, with the report on stdout.
        agreement = JSON.parse(error.stdout);
      }
    }
    const statuses = {};
    if (merged) {
      const strong = JSON.parse(readFileSync(path.join(runs, merged, "strong-mutation.json"), "utf8"));
      for (const verdict of Object.values(strong.mutants)) statuses[verdict.status] = (statuses[verdict.status] ?? 0) + 1;
    }
    // The shards' own verdicts, added up: a merge holds every one of them
    // once, and each mutant is in one shard's share.
    const reached = new Map();
    let shardVerdicts = 0;
    for (const shard of shards) {
      const shardRuns = path.join(shard, "runs");
      if (!existsSync(shardRuns)) continue;
      for (const run of readdirSync(shardRuns)) {
        const file = path.join(shardRuns, run, "strong-mutation.json");
        if (!existsSync(file)) continue;
        for (const id of Object.keys(JSON.parse(readFileSync(file, "utf8")).mutants)) {
          shardVerdicts += 1;
          reached.set(id, (reached.get(id) ?? 0) + 1);
        }
      }
    }
    const inTwoShares = [...reached.values()].filter((times) => times > 1).length;
    const mergedVerdicts = Object.values(statuses).reduce((a, b) => a + b, 0);
    if (crate.supercovShards > 1 && (inTwoShares > 0 || mergedVerdicts !== shardVerdicts)) {
      note += ` shards: ${shardVerdicts} verdicts, ${inTwoShares} mutant(s) in two shares; merged: ${mergedVerdicts}`;
      violations += 1;
    }
    const cargoMutants = {};
    for (const file of outcomes) {
      for (const outcome of JSON.parse(readFileSync(file, "utf8")).outcomes) {
        if (!outcome.scenario?.Mutant) continue;
        cargoMutants[outcome.summary] = (cargoMutants[outcome.summary] ?? 0) + 1;
      }
    }
    const result = {
      crate: crate.name,
      tag: crate.tag,
      shape: crate.shape,
      test: crate.test,
      note,
      cargoMutants: {
        statuses: cargoMutants,
        mutants: Object.values(cargoMutants).reduce((a, b) => a + b, 0),
        time: sum(
          Array.from({ length: crate.truthShards }, (_, index) =>
            times(path.join(artifactsDir, `truth-${crate.name}-${index}`, "time.txt")),
          ),
        ),
      },
      supercov: {
        statuses,
        time: sum(shards.map((shard) => times(path.join(shard, "time.txt")))),
      },
      agreement,
    };
    violations += agreement?.soundnessViolations.length ?? 0;
    violations += agreement?.weak.violations.length ?? 0;
    writeFileSync(path.join(outDir, `${crate.name}.json`), `${JSON.stringify(result, null, 2)}\n`);
    results.push(result);
  }
  writeFileSync(path.join(outDir, "results.json"), `${JSON.stringify(results, null, 2)}\n`);
  rmSync(path.join(outDir, "work"), { recursive: true, force: true });
  const seconds = (time) => `${time.wall.toFixed(1)} / ${time.cpu.toFixed(1)}${time.measured < time.jobs ? ` (${time.measured} of ${time.jobs} jobs)` : ""}`;
  console.log("| Crate | Mutants (cargo-mutants) | Joined | Agree | Soundness violations | Viability disagreements | Only cargo-mutants / only Supercov | Weak violations | cargo-mutants wall / CPU s | Supercov wall / CPU s |");
  console.log("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
  for (const result of results) {
    const a = result.agreement;
    console.log(
      `| ${result.crate} ${result.tag} | ${result.cargoMutants.mutants} | ${a?.matched ?? "-"} | ${a?.agree ?? "-"} | ${a?.soundnessViolations.length ?? "-"} | ${a?.viabilityDisagreements.length ?? "-"} | ${a ? `${a.onlyCargoMutants.length} / ${a.onlySupercov.length}` : "-"} | ${a ? a.weak.violations.length : "-"} | ${seconds(result.cargoMutants.time)} | ${seconds(result.supercov.time)} |${result.note ? ` ${result.note}` : ""}`,
    );
  }
  for (const result of results) {
    const a = result.agreement;
    if (!a) continue;
    for (const v of [...a.soundnessViolations, ...a.viabilityDisagreements]) {
      console.log(`- ${result.crate} \`${v.id}\`: cargo-mutants ${v.cargoMutants}, Supercov ${v.supercov}${v.reason ? ` (${v.reason})` : ""}`);
    }
  }
  process.exitCode = violations > 0 ? 1 : 0;
}
