// A Rust Worker that a `cargo test` suite starts itself is measured, per test.
//
//   node scripts/rust-worker-cargo-test.mjs
//
// A Cargo workspace with a Worker crate, built by worker-build, and a crate
// whose integration tests start `wrangler dev --local` from PATH, ask it
// something and stop it -- as a project tests its Worker in Cloudflare's own
// runtime. The tests find Supercov's `wrangler` first: it gives each `wrangler
// dev` an inspector port and a poller writing into that test's evidence, then
// becomes the real one. Each test must be credited exactly its own path
// through the Worker, the Worker crate must count though `cargo test` never
// compiles it, and the project must be left as it was: no configuration
// pointed at Supercov's entry, no Worker built with probes in it.
// Without worker-build or the wasm target there is nothing to build, and the
// gate says so; a job that installed them sets SUPERCOV_REQUIRE_RUST_WORKER=1.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { delimiter, dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-worker-cargo-test-${process.pid}-${Date.now()}`);
const WRANGLER = '4.151.1';
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

const targets = run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '';
if (process.platform === 'win32' || run(repository, 'worker-build', ['--version']).status !== 0 || !targets.includes('wasm32-unknown-unknown')) {
  if (process.env.SUPERCOV_REQUIRE_RUST_WORKER === '1') {
    console.error('[rust-worker-cargo-test] worker-build or the wasm32-unknown-unknown target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-worker-cargo-test] skipped: needs worker-build and the wasm32-unknown-unknown target');
  process.exit(0);
}

const files = {
  'package.json': `{ "name": "grades", "private": true, "devDependencies": { "wrangler": "${WRANGLER}" } }\n`,
  '.gitignore': 'node_modules/\ntarget/\n.wrangler/\n',
  // Its own workspace: the fixture sits inside Supercov's checkout.
  'Cargo.toml': '[workspace]\nmembers = ["worker", "host"]\nresolver = "2"\n\n[profile.release]\nopt-level = "s"\n',
  'worker/Cargo.toml': '[package]\nname = "grades-worker"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n',
  'worker/wrangler.toml': 'name = "grades-worker"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\n',
  'worker/src/lib.rs': `use worker::*;

pub fn grade(score: u32, curved: bool) -> &'static str {
    if score >= 90 || (curved && score >= 85) {
        "A"
    } else if score >= 50 {
        "pass"
    } else {
        "fail"
    }
}

#[event(fetch)]
async fn fetch(request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = request.url()?;
    if url.path() == "/ready" {
        return Response::ok("ready");
    }
    let score = url
        .query_pairs()
        .find(|(key, _)| key == "score")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let curved = url.query_pairs().any(|(key, _)| key == "curved");
    Response::ok(grade(score, curved))
}
`,
  'host/Cargo.toml': '[package]\nname = "host"\nversion = "0.1.0"\nedition = "2021"\n',
  'host/src/lib.rs': '//! The suite that starts the Worker.\n',
  'host/tests/worker.rs': `use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// One Worker at a time: each \`wrangler dev\` builds the crate in place.
fn exclusive() -> std::fs::File {
    let file = std::fs::File::create(std::env::temp_dir().join("grades-worker-gate.lock")).unwrap();
    file.lock().unwrap();
    file
}

struct Dev {
    child: Child,
    port: u16,
}

impl Drop for Dev {
    // wrangler starts Cloudflare's runtime as processes of its own, in its group.
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", &format!("-{}", self.child.id())]).status();
        let _ = self.child.wait();
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

fn serve() -> Dev {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new("wrangler")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../worker"))
        .args(["dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("wrangler on PATH");
    let dev = Dev { child, port };
    for _ in 0..1200 {
        if get(port, "/ready").as_deref() == Some("ready") {
            return dev;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("wrangler dev never answered");
}

#[test]
fn a_curved_score_is_an_a() {
    let _one = exclusive();
    let dev = serve();
    assert_eq!(get(dev.port, "/?score=87&curved").as_deref(), Some("A"));
}

#[test]
fn a_low_score_fails() {
    let _one = exclusive();
    let dev = serve();
    assert_eq!(get(dev.port, "/?score=10").as_deref(), Some("fail"));
}

#[test]
fn a_test_that_starts_no_worker() {
    assert_eq!(1 + 1, 2);
}
`,
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

  // The suite finds wrangler on PATH, as a project's does.
  const path = `${resolve(root, 'node_modules/.bin')}${delimiter}${process.env.PATH}`;
  const measured = run(root, binary, ['--', 'cargo', 'test'], { PATH: path });
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  assert.match(measured.stderr, /Rust coverage: 3 test\(s\)/u, measured.stderr);
  assert.doesNotMatch(measured.stderr, /did not compile[^\n]*worker\/src\/lib\.rs/u, measured.stderr);

  const id = JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs[0].id;
  const summary = run(root, binary, ['runs', id]);
  assert.doesNotMatch(summary.stdout + summary.stderr, /stale/iu, summary.stdout + summary.stderr);
  const lines = (name) => [...new Set(run(root, binary, ['runs', id, 'test', name, '--limit', '0']).stdout.match(/worker\/src\/lib\.rs:\d+/gu) ?? [])].sort();
  const curved = lines('a_curved_score_is_an_a');
  const low = lines('a_low_score_fails');
  // Each test is credited its own path through `grade`, exactly.
  assert.ok(curved.includes('worker/src/lib.rs:4') && curved.includes('worker/src/lib.rs:5'), curved.join(' '));
  assert.ok(!curved.includes('worker/src/lib.rs:9'), curved.join(' '));
  assert.ok(low.includes('worker/src/lib.rs:9'), low.join(' '));
  assert.ok(!low.includes('worker/src/lib.rs:5'), low.join(' '));
  assert.deepEqual(lines('a_test_that_starts_no_worker'), []);
  const pass = run(root, binary, ['runs', id, 'line', 'worker/src/lib.rs:7']).stdout;
  assert.match(pass, /NOT COVERED/u, pass);
  const decision = run(root, binary, ['runs', id, 'decision', 'worker/src/lib.rs:4']).stdout;
  assert.match(decision, /FF- -> F +tests=1/u, decision);
  assert.match(decision, /FTT -> T +tests=1/u, decision);
  const scope = JSON.parse(run(root, binary, ['runs', id, 'scope', '--json', '--limit', '0']).stdout).data.entries;
  assert.equal(scope.find((entry) => entry.file === 'worker/src/lib.rs')?.status, 'included', JSON.stringify(scope));

  // The project is as it was: the copy alone was pointed at Supercov's
  // entry, and the Worker built with probes stayed there.
  const status = run(root, 'git', ['status', '--porcelain', '--untracked-files=all']).stdout;
  assert.equal(status.trim(), '', status);
  assert.ok(!existsSync(resolve(root, 'worker/build')), 'a Worker built in the copy was returned to the project');
  console.log('[rust-worker-cargo-test] a Rust Worker a cargo test suite starts with wrangler dev is measured, each test credited its own work exactly');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-worker-cargo-test-')))
    rmSync(root, { recursive: true, force: true });
}
