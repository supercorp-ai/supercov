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
// left as the project made it.
// Without worker-build or the wasm target there is nothing to build, and the
// gate says so; a job that installed them sets SUPERCOV_REQUIRE_RUST_WORKER=1.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
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

const worker = (name) => `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n`;
const PLANE = `use worker::*;

#[event(fetch)]
async fn fetch(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    if request.path() == "/ready" {
        return Response::ok("ready");
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
        let next = count + self.step;
        storage.put("count", next).await?;
        Response::ok(next.to_string())
    }
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
  'plane/Cargo.toml': worker('plane'),
  'plane/wrangler.toml': 'name = "plane"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\n\n[[durable_objects.bindings]]\nname = "COUNTER"\nclass_name = "Counter"\n\n[[migrations]]\ntag = "v1"\nnew_sqlite_classes = ["Counter"]\n',
  'plane/src/lib.rs': PLANE,
  'runners/Cargo.toml': worker('runners'),
  'runners/wrangler.toml': 'name = "runners"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\n',
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

/// The Worker in \`crate_name\`, or none where it is not built: a test kit
/// that will not run an unbuilt Worker.
fn serve(crate_name: &str, vars: &[&str]) -> Option<Dev> {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(crate_name);
    if !directory.join("build").exists() {
        return None;
    }
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let storage = std::env::temp_dir().join(format!("planes-storage-{}-{port}", std::process::id()));
    let mut command = Command::new("wrangler");
    command
        .current_dir(&directory)
        .args(["dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false"])
        .args(["--persist-to", &storage.display().to_string()]);
    for var in vars {
        command.args(["--var", var]);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("wrangler on PATH");
    let dev = Dev { child, port, storage };
    for _ in 0..1200 {
        if get(port, "/ready").as_deref() == Some("ready") {
            return Some(dev);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("wrangler dev never answered");
}

#[test]
fn counts_through_the_durable_object() {
    let _one = exclusive();
    let Some(dev) = serve("plane", &["STEP:2"]) else { return };
    assert_eq!(get(dev.port, "/count").as_deref(), Some("2"));
    assert_eq!(get(dev.port, "/count").as_deref(), Some("4"));
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
fn a_test_that_starts_no_worker() {
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
    const measured = run(root, binary, ['--', 'cargo', 'test', '--workspace', '--', ...filter], { PATH: path });
    assert.equal(measured.status, 0, `${filter.join(' ')}\n${measured.stdout}${measured.stderr}`);
    return { measured, id: JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs[0].id };
  };
  const { measured, id } = measure();
  assert.match(measured.stderr, /Rust coverage: 4 test\(s\)/u, measured.stderr);
  const lines = (runId, name, file) => [...new Set(run(root, binary, ['runs', runId, 'test', name, '--limit', '0']).stdout.match(new RegExp(`${file.replace(/[/.]/gu, '\\$&')}:\\d+`, 'gu')) ?? [])];
  const at = (file, source, text) => `${file}:${lineOf(source, text)}`;

  // Each test its own path through the Durable Object's methods: neither
  // skipped for want of a build, nor credited the other's branch.
  const counted = lines(id, 'counts_through_the_durable_object', 'plane/src/lib.rs');
  const reset = lines(id, 'resets_the_count', 'plane/src/lib.rs');
  const next = at('plane/src/lib.rs', PLANE, 'let next = count + self.step;');
  const zero = at('plane/src/lib.rs', PLANE, 'return Response::ok("0");');
  const step = at('plane/src/lib.rs', PLANE, '.var("STEP")');
  assert.ok(counted.includes(next) && counted.includes(step), `the counting test: ${counted.join(' ')}`);
  assert.ok(!counted.includes(zero), `the counting test: ${counted.join(' ')}`);
  assert.ok(reset.includes(zero) && !reset.includes(next), `the resetting test: ${reset.join(' ')}`);
  const decision = run(root, binary, ['runs', id, 'decision', at('plane/src/lib.rs', PLANE, 'if request.path() == "/reset" || count >= 100')]).stdout;
  assert.match(decision, /FF -> F +tests=1/u, decision);
  assert.match(decision, /T- -> T +tests=1/u, decision);
  const busy = lines(id, 'a_runner_with_two_slots_is_busy', 'runners/src/lib.rs');
  assert.ok(busy.includes(at('runners/src/lib.rs', RUNNERS, '"busy"')), `the runner test: ${busy.join(' ')}`);
  assert.ok(!busy.includes(at('runners/src/lib.rs', RUNNERS, '"full"')), `the runner test: ${busy.join(' ')}`);
  assert.deepEqual(lines(id, 'a_test_that_starts_no_worker', 'plane/src/lib.rs'), []);
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
