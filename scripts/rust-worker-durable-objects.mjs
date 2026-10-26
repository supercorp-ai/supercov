// Two Rust Workers, one with a Durable Object, that `cargo test` starts under
// `wrangler dev --local` the way a control plane's test kit does.
//
//   node scripts/rust-worker-durable-objects.mjs
//
// A Cargo workspace with two Worker crates built by worker-build: `plane`,
// whose fetch hands every request to a Durable Object (`#[durable_object]`)
// that keeps a count in its storage, and `runners`. Their `build/` is
// git-ignored and built in the project beforehand, and the tests start a
// Worker only where its `build/` is there, as a test kit that refuses to run
// an unbuilt Worker does; they pass `--var` and `--persist-to` as such a kit
// does. Each test must be credited its own path through the Durable Object's
// methods and the other Worker's code, each crate counted once, runs of
// different tests merged under one denominator, and the project's own build
// left as the project made it. The suite runs as `cargo test -p host`, which
// never compiles the Workers' crates itself: they count all the same, as
// worker-build compiles them -- for wasm, as a release, with their default
// features. One test starts both Workers in a single \`wrangler dev\`
// (\`-c plane/... -c runners/...\`), reaching the second only through the
// first's service binding: the second is credited to it too.
// Without worker-build or the wasm target there is nothing to build, and the
// gate says so; a job that installed them sets SUPERCOV_REQUIRE_RUST_WORKER=1.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { delimiter, dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-worker-durable-objects-${process.pid}-${Date.now()}`);
const WRANGLER = '4.151.1';
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

const targets = run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '';
if (process.platform === 'win32' || run(repository, 'worker-build', ['--version']).status !== 0 || !targets.includes('wasm32-unknown-unknown')) {
  if (process.env.SUPERCOV_REQUIRE_RUST_WORKER === '1') {
    console.error('[rust-worker-durable-objects] worker-build or the wasm32-unknown-unknown target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-worker-durable-objects] skipped: needs worker-build and the wasm32-unknown-unknown target');
  process.exit(0);
}

const worker = (name, extra = '') => `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n${extra}`;
const PLANE = `use worker::*;

#[event(fetch)]
async fn fetch(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    if request.path() == "/ready" {
        return Response::ok("ready");
    }
    if request.path() == "/runners" {
        return env.service("RUNNERS")?.fetch(request.url()?.to_string(), None).await;
    }
    if request.path() == "/build" {
        return Response::ok(format!("{} {}", runtime(), build()));
    }
    let stub = env.durable_object("COUNTER")?.id_from_name("one")?.get_stub()?;
    stub.fetch_with_request(request).await
}

#[durable_object]
pub struct Counter {
    state: State,
    step: u32,
}

impl DurableObject for Counter {
    fn new(state: State, env: Env) -> Self {
        let step = env
            .var("STEP")
            .ok()
            .and_then(|value| value.to_string().parse().ok())
            .unwrap_or(1);
        Counter { state, step }
    }

    async fn fetch(&self, request: Request) -> Result<Response> {
        let storage = self.state.storage();
        let count = storage.get::<u32>("count").await?.unwrap_or(0);
        if request.path() == "/reset" || count >= 100 {
            storage.put("count", 0u32).await?;
            return Response::ok("0");
        }
        let next = audited(count + self.step);
        storage.put("count", next).await?;
        Response::ok(next.to_string())
    }
}

// What the Worker's build compiles is what counts: worker-build builds for
// wasm, as a release, with the crate's default features.
#[cfg(target_arch = "wasm32")]
fn runtime() -> &'static str {
    "wasm"
}

#[cfg(not(target_arch = "wasm32"))]
fn runtime() -> &'static str {
    "native"
}

#[cfg(debug_assertions)]
fn build() -> &'static str {
    "debug"
}

#[cfg(not(debug_assertions))]
fn build() -> &'static str {
    "release"
}

#[cfg(feature = "audit")]
fn audited(count: u32) -> u32 {
    count
}

#[cfg(not(feature = "audit"))]
fn audited(count: u32) -> u32 {
    count + 1000
}
`;
const RUNNERS = `use worker::*;

pub fn label(slots: u32) -> &'static str {
    if slots == 0 {
        "full"
    } else if slots < 3 {
        "busy"
    } else {
        "free"
    }
}

#[event(fetch)]
async fn fetch(request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    if request.path() == "/ready" {
        return Response::ok("ready");
    }
    let slots = request
        .url()?
        .query_pairs()
        .find(|(key, _)| key == "slots")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    Response::ok(label(slots))
}
`;
const files = {
  'package.json': `{ "name": "planes", "private": true, "devDependencies": { "wrangler": "${WRANGLER}" } }\n`,
  // The Workers' builds are the project's own, made before the tests and
  // never committed, as a project that embeds them keeps them.
  '.gitignore': 'node_modules/\ntarget/\n.wrangler/\nbuild/\n',
  'Cargo.toml': '[workspace]\nmembers = ["plane", "runners", "host"]\nresolver = "2"\n\n[profile.release]\nopt-level = "s"\n',
  'plane/Cargo.toml': worker('plane', '\n[features]\ndefault = ["audit"]\naudit = []\n'),
  // Started from the workspace root, as a test kit of a monorepo starts
  // them: wrangler runs a build where it was started, unless told where.
  'plane/wrangler.toml': 'name = "plane"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\ncwd = "plane"\n\n[[durable_objects.bindings]]\nname = "COUNTER"\nclass_name = "Counter"\n\n[[migrations]]\ntag = "v1"\nnew_sqlite_classes = ["Counter"]\n\n[[services]]\nbinding = "RUNNERS"\nservice = "runners"\n',
  'plane/src/lib.rs': PLANE,
  'runners/Cargo.toml': worker('runners'),
  'runners/wrangler.toml': 'name = "runners"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\ncwd = "runners"\n',
  'runners/src/lib.rs': RUNNERS,
  'host/Cargo.toml': '[package]\nname = "host"\nversion = "0.1.0"\nedition = "2021"\n',
  'host/src/lib.rs': '//! The suite that starts the Workers.\n',
  'host/tests/workers.rs': `use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// One Worker at a time: each \`wrangler dev\` builds its crate in place.
fn exclusive() -> std::fs::File {
    let file = std::fs::File::create(std::env::temp_dir().join("planes-worker-gate.lock")).unwrap();
    file.lock().unwrap();
    file
}

struct Dev {
    child: Child,
    port: u16,
    storage: std::path::PathBuf,
}

impl Drop for Dev {
    // wrangler starts Cloudflare's runtime as processes of its own, in its
    // group (\`--\`: a bare \`-<pgid>\` is \`kill(-1)\` to Linux's procps).
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", "--", &format!("-{}", self.child.id())]).status();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.storage);
    }
}

fn get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok()?;
    write!(stream, "GET {path} HTTP/1.1\\r\\nhost: 127.0.0.1\\r\\nconnection: close\\r\\n\\r\\n").ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    let (head, body) = text.split_once("\\r\\n\\r\\n")?;
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    if !head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        return Some(body.trim().to_owned());
    }
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\\r\\n") {
        let size = usize::from_str_radix(size.trim(), 16).ok()?;
        if size == 0 {
            break;
        }
        out.push_str(after.get(..size)?);
        rest = after.get(size + 2..)?;
    }
    Some(out)
}

fn workspace() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The Worker in \`crate_name\`, or none where it is not built: a test kit
/// that will not run an unbuilt Worker.
fn serve(crate_name: &str, vars: &[&str]) -> Option<Dev> {
    let configs = ["-c".to_owned(), format!("{crate_name}/wrangler.toml")];
    workspace().join(crate_name).join("build").exists().then(|| start(&workspace(), &configs, vars))
}

/// Several Workers in one \`wrangler dev\`, the first serving and the others
/// reached through its service bindings; none unless each is built.
fn serve_together(crates: &[&str]) -> Option<Dev> {
    if crates.iter().any(|name| !workspace().join(name).join("build").exists()) {
        return None;
    }
    let configs = crates.iter().flat_map(|name| ["-c".to_owned(), format!("{name}/wrangler.toml")]).collect::<Vec<_>>();
    Some(start(&workspace(), &configs, &[]))
}

fn start(directory: &std::path::Path, configs: &[String], vars: &[&str]) -> Dev {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let storage = std::env::temp_dir().join(format!("planes-storage-{}-{port}", std::process::id()));
    let mut command = Command::new("wrangler");
    command
        .current_dir(directory)
        .arg("dev")
        .args(configs)
        .args(["--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false"])
        .args(["--persist-to", &storage.display().to_string()]);
    for var in vars {
        command.args(["--var", var]);
    }
    // What wrangler says, kept to say why a Worker never answered.
    let log = std::env::temp_dir().join(format!("planes-wrangler-{}-{port}.log", std::process::id()));
    let output = std::fs::File::create(&log).unwrap();
    let child = command
        .stdin(Stdio::null())
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .process_group(0)
        .spawn()
        .expect("wrangler on PATH");
    let dev = Dev { child, port, storage };
    for _ in 0..1200 {
        if get(port, "/ready").as_deref() == Some("ready") {
            return dev;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let tail = said.lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    panic!("wrangler dev never answered:\n{tail}");
}

#[test]
fn counts_through_the_durable_object() {
    let _one = exclusive();
    let Some(dev) = serve("plane", &["STEP:2"]) else { return };
    assert_eq!(get(dev.port, "/count").as_deref(), Some("2"));
    assert_eq!(get(dev.port, "/count").as_deref(), Some("4"));
    assert_eq!(get(dev.port, "/build").as_deref(), Some("wasm release"));
}

#[test]
fn resets_the_count() {
    let _one = exclusive();
    let Some(dev) = serve("plane", &[]) else { return };
    assert_eq!(get(dev.port, "/reset").as_deref(), Some("0"));
}

#[test]
fn a_runner_with_two_slots_is_busy() {
    let _one = exclusive();
    let Some(dev) = serve("runners", &[]) else { return };
    assert_eq!(get(dev.port, "/?slots=2").as_deref(), Some("busy"));
}

#[test]
fn a_runner_reached_through_the_plane_is_busy() {
    let _one = exclusive();
    let Some(dev) = serve_together(&["plane", "runners"]) else { return };
    assert_eq!(get(dev.port, "/runners?slots=2").as_deref(), Some("busy"));
}

/// A test kit's own configuration, written for the test outside the crate,
/// whose \`main\` is the test's own module loading the built Worker: the
/// shape of a harness that wraps a Worker with stand-ins of its own.
#[test]
fn a_harness_config_outside_the_crate_runs_the_built_worker() {
    let _one = exclusive();
    let crate_directory = workspace().join("runners").canonicalize().unwrap();
    if !crate_directory.join("build").exists() {
        return;
    }
    let harness = std::env::temp_dir().join(format!("planes-harness-{}", std::process::id()));
    std::fs::create_dir_all(&harness).unwrap();
    let shim = crate_directory.join("build/worker/shim.mjs");
    std::fs::write(
        harness.join("harness.mjs"),
        format!(
            "// The test's own module, loading the built Worker.\\nexport * from \\"{0}\\";\\nexport {{ default }} from \\"{0}\\";\\n",
            shim.display()
        ),
    )
    .unwrap();
    let config = format!(
        "name = \\"harness\\"\\nmain = \\"harness.mjs\\"\\ncompatibility_date = \\"2026-10-01\\"\\n\\n[build]\\ncommand = \\"worker-build --release\\"\\ncwd = \\"{0}\\"\\nwatch_dir = \\"{0}/src\\"\\n",
        crate_directory.display()
    );
    std::fs::write(harness.join("wrangler.toml"), &config).unwrap();
    let dev = start(&harness, &[], &[]);
    assert_eq!(get(dev.port, "/?slots=2").as_deref(), Some("busy"));
    drop(dev);
    // The test's configuration is as the test wrote it.
    assert_eq!(std::fs::read_to_string(harness.join("wrangler.toml")).unwrap(), config);
    let _ = std::fs::remove_dir_all(&harness);
}

#[test]
fn a_test_that_starts_no_worker() {
    println!("printed by a test that starts no worker, measured in {}", std::env::var("SUPERCOV_RUN_ID").unwrap_or_default());
    assert_eq!(1 + 1, 2);
}
`,
};

/// The 1-based line of `source` that holds `text`.
const lineOf = (source, text) => {
  const index = source.split('\n').findIndex((line) => line.includes(text));
  assert.ok(index >= 0, `no line holds ${text}`);
  return index + 1;
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const installed = run(root, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(run(root, 'cargo', ['generate-lockfile', '-q']).status, 0);
  assert.equal(run(root, 'git', ['init', '-q']).status, 0);
  assert.equal(run(root, 'git', ['add', '-A']).status, 0);
  assert.equal(run(root, 'git', ['-c', 'user.name=gate', '-c', 'user.email=gate@example.com', 'commit', '-qm', 'fixture']).status, 0);
  // The project builds its Workers itself, before any test.
  for (const crate of ['plane', 'runners']) {
    const built = run(resolve(root, crate), 'worker-build', ['--release']);
    assert.equal(built.status, 0, `${crate}: ${built.stdout}${built.stderr}`);
  }
  const wasm = (crate) => {
    const directory = resolve(root, crate, 'build/worker');
    return readdirSync(directory).filter((name) => name.endsWith('.wasm')).map((name) => readFileSync(resolve(directory, name)));
  };
  const before = { plane: wasm('plane'), runners: wasm('runners') };

  const path = `${resolve(root, 'node_modules/.bin')}${delimiter}${process.env.PATH}`;
  const measure = (filter = []) => {
    const measured = run(root, binary, ['--', 'cargo', 'test', '-p', 'host', '--', ...filter], { PATH: path });
    assert.equal(measured.status, 0, `${filter.join(' ')}\n${measured.stdout}${measured.stderr}`);
    return { measured, id: JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs[0].id };
  };
  const { measured, id } = measure();
  console.log(measured.stderr.split('\n').filter((line) => line.includes('synced')).join('\n'));
  assert.match(measured.stderr, /Rust coverage: 6 test\(s\)/u, measured.stderr);
  const lines = (runId, name, file) => [...new Set(run(root, binary, ['runs', runId, 'test', name, '--limit', '0']).stdout.match(new RegExp(`${file.replace(/[/.]/gu, '\\$&')}:\\d+`, 'gu')) ?? [])];
  const at = (file, source, text) => `${file}:${lineOf(source, text)}`;

  // Each test its own path through the Durable Object's methods: neither
  // skipped for want of a build, nor credited the other's branch.
  const counted = lines(id, 'counts_through_the_durable_object', 'plane/src/lib.rs');
  const reset = lines(id, 'resets_the_count', 'plane/src/lib.rs');
  const next = at('plane/src/lib.rs', PLANE, 'let next = audited(count + self.step);');
  const zero = at('plane/src/lib.rs', PLANE, 'return Response::ok("0");');
  const step = at('plane/src/lib.rs', PLANE, 'let step = env');
  assert.ok(counted.includes(next) && counted.includes(step), `the counting test: ${counted.join(' ')}`);
  assert.ok(!counted.includes(zero), `the counting test: ${counted.join(' ')}`);
  assert.ok(reset.includes(zero) && !reset.includes(next), `the resetting test: ${reset.join(' ')}`);
  const decision = run(root, binary, ['runs', id, 'decision', at('plane/src/lib.rs', PLANE, 'if request.path() == "/reset" || count >= 100')]).stdout;
  assert.match(decision, /FF -> F +tests=1/u, decision);
  assert.match(decision, /T- -> T +tests=1/u, decision);
  // The Worker's build decides what is code: what it compiles counts and
  // ran, what it leaves out is no code of the run.
  for (const [gate, compiled] of [
    ['#[cfg(target_arch = "wasm32")]', true],
    ['#[cfg(not(debug_assertions))]', true],
    ['#[cfg(feature = "audit")]', true],
    ['#[cfg(not(target_arch = "wasm32"))]', false],
    ['#[cfg(debug_assertions)]', false],
    ['#[cfg(not(feature = "audit"))]', false],
  ]) {
    const item = `plane/src/lib.rs:${lineOf(PLANE, gate) + 1}`;
    const shown = run(root, binary, ['runs', id, 'line', item]).stdout;
    if (compiled) assert.ok(counted.includes(item), `${gate}: ${counted.join(' ')}`);
    else assert.match(shown, /Nothing on this line is code the run measures/u, `${gate}\n${shown}`);
  }
  const busy = lines(id, 'a_runner_with_two_slots_is_busy', 'runners/src/lib.rs');
  assert.ok(busy.includes(at('runners/src/lib.rs', RUNNERS, '"busy"')), `the runner test: ${busy.join(' ')}`);
  assert.ok(!busy.includes(at('runners/src/lib.rs', RUNNERS, '"full"')), `the runner test: ${busy.join(' ')}`);
  // A Worker that runs beside another in one \`wrangler dev\`, reached only
  // through the first's service binding, is read too.
  const together = lines(id, 'a_runner_reached_through_the_plane_is_busy', 'runners/src/lib.rs');
  assert.ok(together.includes(at('runners/src/lib.rs', RUNNERS, '"busy"')), `the runner beside the plane: ${together.join(' ')}`);
  const forwarded = lines(id, 'a_runner_reached_through_the_plane_is_busy', 'plane/src/lib.rs');
  assert.ok(forwarded.includes(at('plane/src/lib.rs', PLANE, 'env.service("RUNNERS")')), `the plane forwarding: ${forwarded.join(' ')}`);
  // A configuration a test writes outside the crate, whose \`main\` is its
  // own module loading the built Worker, is measured as the crate's is.
  const harness = lines(id, 'a_harness_config_outside_the_crate_runs_the_built_worker', 'runners/src/lib.rs');
  assert.ok(harness.includes(at('runners/src/lib.rs', RUNNERS, '"busy"')), `the harness test: ${harness.join(' ')}`);
  assert.deepEqual(lines(id, 'a_test_that_starts_no_worker', 'plane/src/lib.rs'), []);
  // wrangler's state stays in the copy.
  for (const state of ['.wrangler', 'plane/.wrangler', 'runners/.wrangler']) {
    assert.ok(!existsSync(resolve(root, state)), `${state} came back to the project`);
  }
  // Asked not to capture, a test's output is shown.
  const shown = run(root, binary, ['--', 'cargo', 'test', '-p', 'host', '--', '--nocapture', 'a_test_that_starts_no_worker'], { PATH: path });
  assert.equal(shown.status, 0, shown.stdout + shown.stderr);
  // And the test knows it is measured, and in which run.
  assert.match(shown.stdout, /printed by a test that starts no worker, measured in run_[0-9a-f]{16}/u, shown.stdout);
  const scope = JSON.parse(run(root, binary, ['runs', id, 'scope', '--json', '--limit', '0']).stdout).data.entries;
  for (const file of ['plane/src/lib.rs', 'runners/src/lib.rs']) {
    assert.equal(scope.filter((entry) => entry.file === file).length, 1, `${file} counted once: ${JSON.stringify(scope)}`);
    assert.equal(scope.find((entry) => entry.file === file)?.status, 'included', `${file}: ${JSON.stringify(scope)}`);
  }

  // Runs of different tests, as a suite measured in parts, merge: one
  // denominator, each Worker's work kept.
  const plane = measure(['counts_through_the_durable_object']);
  const runners = measure(['a_runner_with_two_slots_is_busy']);
  const merged = run(root, binary, ['merge', plane.id, runners.id]);
  assert.equal(merged.status, 0, merged.stdout + merged.stderr);
  const total = run(root, binary, ['runs', 'latest', 'line', next]).stdout + run(root, binary, ['runs', 'latest', 'line', at('runners/src/lib.rs', RUNNERS, '"busy"')]).stdout;
  assert.doesNotMatch(total, /NOT COVERED|NOT MEASURED/u, total);

  // The project's own builds are as it made them: the probes were built in
  // the copy alone.
  assert.deepEqual(wasm('plane'), before.plane, 'the project\'s plane build was replaced');
  assert.deepEqual(wasm('runners'), before.runners, 'the project\'s runners build was replaced');
  const status = run(root, 'git', ['status', '--porcelain', '--untracked-files=all']).stdout;
  assert.equal(status.trim(), '', status);
  console.log('[rust-worker-durable-objects] two Rust Workers, one with a Durable Object, started by cargo test from their own builds, are measured per test and merge across runs');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-worker-durable-objects-')))
    rmSync(root, { recursive: true, force: true });
}
