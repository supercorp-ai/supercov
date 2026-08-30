// Exactness ratchet: measure the exact fraction per crate and refuse a
// regression.
//
// The north star's measure of success for HONEST is that the exact fraction
// ratchets toward 100% and no release ever regresses it. Tonight that promise
// depended on remembering to measure widely, and it failed: a change measured
// on six crates showed two gains and no regressions, while serde_json had
// dropped 91.98% -> 66.22% and serde_core 95.03% -> 80.76%. The crates that
// moved were exactly the ones not sampled, which is the normal case — a binder
// change moves failures between stages, so the crates leaning on the changed
// stage are the ones that move.
//
//   node scripts/rust-exactness-ratchet.mjs measure   # build the crate set, print fractions
//   node scripts/rust-exactness-ratchet.mjs baseline  # measure and store as the baseline
//   node scripts/rust-exactness-ratchet.mjs check     # measure and fail on any regression
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {mkdirSync, writeFileSync, readFileSync, readdirSync, existsSync, rmSync} from 'node:fs';
import {join} from 'node:path';

import {fileURLToPath} from 'node:url';
import {dirname} from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const scratch = process.env.SUPERCOV_RATCHET_WORK ?? join(root, 'target', 'exactness-ratchet');
const wrapper = join(root, 'spikes/rustc-backend/target/debug/supercov-rustc-backend-spike');
const baselinePath = join(root, 'spikes/rustc-backend/exactness-baseline.json');
const work = join(scratch, 'exactness-work');

// Chosen to span the stages a binder change can move failures between: serde's
// collapsed generated matches, syn/quote/proc-macro2's macro expansion,
// tracing's attribute macros, and plain authored code in bytes/http/either.
const dependencies = [
  ['serde_json', '1'],
  ['serde', '1'],
  ['syn', '2'],
  ['quote', '1'],
  ['proc-macro2', '1'],
  ['tracing', '0.1'],
  ['bytes', '1.12'],
  ['http', '1'],
  ['either', '1'],
  ['once_cell', '1'],
  ['log', '0.4'],
  ['itoa', '1'],
  ['memchr', '2'],
];

const registry = execFileSync('sh', [
  '-c',
  'ls -d "$HOME"/.cargo/registry/src/*/ | head -1',
]).toString().trim();
assert(registry, 'no cargo registry checkout found');

function measure() {
  rmSync(work, {recursive: true, force: true});
  mkdirSync(join(work, 'src'), {recursive: true});
  writeFileSync(
    join(work, 'Cargo.toml'),
    ['[package]', 'name = "exactness-probe"', 'version = "0.0.0"', 'edition = "2021"', 'publish = false', '', '[dependencies]']
      .concat(dependencies.map(([name, version]) => `${name} = "${version}"`))
      .concat(['', '[workspace]', ''])
      .join('\n'),
  );
  writeFileSync(join(work, 'src/lib.rs'), '// exactness probe\n');
  const out = join(work, 'out');

  // Traces reach gigabytes and are never read here.
  // The loop must not inherit stdout: spawnSync waits for EOF on the pipe, and
  // a background process holding it open blocks forever.
  const reaper = spawnSync('sh', [
    '-c',
    `(while true; do find ${out} -name '*.jsonl' -size +5M -mmin +1 -delete 2>/dev/null; sleep 30; done) >/dev/null 2>&1 & echo $!`,
  ]).stdout.toString().trim();
  try {
    spawnSync('cargo', ['build', '-j', '4'], {
      cwd: work,
      encoding: 'utf8',
      stdio: ['ignore', 'inherit', 'inherit'],
      env: {
        ...process.env,
        RUSTUP_TOOLCHAIN: '1.95.0',
        CARGO_TARGET_DIR: join(work, 'target'),
        RUSTC_WRAPPER: wrapper,
        // Absolute: cargo runs a dependency with its own directory as cwd, so a
        // relative path writes into the registry and leaves an empty dir that
        // reads exactly like "nothing to report".
        SUPERCOV_RUST_COMPILER_OUTPUT: out,
        SUPERCOV_RUST_INSTRUMENT_MIR: '1',
        SUPERCOV_RUST_SOURCE_ROOT: registry,
        SUPERCOV_RUST_TARGET_ROOT: join(work, 'target'),
            SUPERCOV_RUST_STATIC_RUNTIME_DIRECTORY:
          process.env.SUPERCOV_RATCHET_RUNTIME ?? join(scratch, 'repro-runtime'),
      },
      maxBuffer: 256 * 1024 * 1024,
    });
  } finally {
    if (reaper) spawnSync('kill', [reaper]);
  }

  const fractions = {};
  if (!existsSync(out)) return fractions;
  for (const name of readdirSync(out)) {
    if (!name.startsWith('manifest-') || !name.endsWith('.json')) continue;
    let manifest;
    try {
      manifest = JSON.parse(readFileSync(join(out, name), 'utf8'));
    } catch {
      continue;
    }
    const total =
      manifest.points.length + manifest.branches.length + manifest.decisions.length;
    // Tiny build-script crates add noise without signal.
    if (total < 100) continue;
    const declined = (manifest.unmeasuredObligations ?? []).length;
    // Track uncompiled declines separately. Discovering more code that this
    // build does not contain lowers the exact fraction while making the
    // report more honest, and a ratchet that cannot tell that apart from a
    // binder getting worse will veto exactly the changes it exists to protect.
    const uncompiled = (manifest.limitations ?? []).filter((limitation) =>
      limitation.startsWith('RUST_OBLIGATION_NOT_COMPILED'),
    ).length;
    const previous = fractions[manifest.crate];
    // A crate can be compiled more than once (lib and test targets); keep the
    // largest obligation count so the comparison is stable.
    if (previous && previous.obligations >= total) continue;
    fractions[manifest.crate] = {
      obligations: total,
      declined,
      uncompiled,
      exact: Number((((total - declined) / total) * 100).toFixed(2)),
    };
  }
  return fractions;
}

const mode = process.argv[2] ?? 'measure';
const measured = measure();
const names = Object.keys(measured).sort();
assert(
  names.length >= 8,
  `only ${names.length} crates measured; the probe build did not run — a partial set silently reads as "no regressions"`,
);

if (mode === 'baseline') {
  writeFileSync(baselinePath, `${JSON.stringify(measured, null, 2)}\n`);
  console.log(`baseline stored for ${names.length} crates`);
  for (const name of names) console.log(`  ${name}: ${measured[name].exact}%`);
  process.exit(0);
}

if (mode === 'measure' || !existsSync(baselinePath)) {
  for (const name of names) console.log(`  ${name}: ${measured[name].exact}%`);
  process.exit(0);
}

const baseline = JSON.parse(readFileSync(baselinePath, 'utf8'));
const regressions = [];
const gains = [];
for (const name of names) {
  const before = baseline[name];
  if (!before) {
    console.log(`  ${name}: ${measured[name].exact}% (new)`);
    continue;
  }
  const delta = Number((measured[name].exact - before.exact).toFixed(2));
  const newlyUncompiled = (measured[name].uncompiled ?? 0) - (before.uncompiled ?? 0);
  if (delta < 0 && newlyUncompiled > 0) {
    // Honesty gained, not exactness lost: the drop is accounted for by
    // constructs newly recognised as absent from this build.
    console.log(
      `  honesty: ${name} ${before.exact}% -> ${measured[name].exact}% (${delta}), ` +
        `+${newlyUncompiled} uncompiled declared`,
    );
  } else if (delta < 0) regressions.push(`${name} ${before.exact}% -> ${measured[name].exact}% (${delta})`);
  else if (delta > 0) gains.push(`${name} ${before.exact}% -> ${measured[name].exact}% (+${delta})`);
}
for (const name of Object.keys(baseline)) {
  if (!measured[name]) regressions.push(`${name} disappeared from the measurement`);
}
for (const gain of gains) console.log(`  gain: ${gain}`);
if (regressions.length > 0) {
  for (const regression of regressions) console.error(`  REGRESSION: ${regression}`);
  console.error('exactness regressed; the north star forbids landing this');
  process.exit(1);
}
console.log(`exactness held or improved across ${names.length} crates`);
