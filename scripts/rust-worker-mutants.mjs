// A Rust Worker that a `cargo test` suite starts with `wrangler dev`, built by
// a build script, gets exact strong verdicts.
//
//   node scripts/rust-worker-mutants.mjs
//
// The Worker's `[build] command` is `./build.sh`, which runs worker-build; a
// crate's integration tests start `wrangler dev --local` from PATH and ask it
// something. Measured, then a strong pass over the run:
//
// - a mutant of `grade()`'s comparison is killed: each test's `wrangler dev`
//   serves the Worker built once from the pass's switched copy, with the
//   test's mutant switched on;
// - a mutant of `shout()`, which a test reaches without checking what it
//   gives, survives, and is a survivor because it ran;
// - a mutant of `hidden()`, which only a test reaches that, during the pass,
//   starts the real wrangler by its own path (a Worker built apart from the
//   pass), is `no-coverage`, `not-reached-mutated`: its tests never ran it
//   mutated, so they could not have failed.
//
// Without worker-build or the wasm target there is nothing to build, and the
// gate says so; a job that installed them sets SUPERCOV_REQUIRE_RUST_WORKER=1.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { delimiter, dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-worker-mutants-${process.pid}-${Date.now()}`);
const WRANGLER = '4.151.1';
const run = (cwd, command, args, env = {}, timeout = 0) => spawnSync(command, args, { cwd, encoding: 'utf8', maxBuffer: 1 << 28, timeout, killSignal: 'SIGKILL', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
// Each step ends in time, saying what it did so far, rather than holding the job.
const LIMIT = 6 * 60 * 1000;
const started = Date.now();
const took = () => `${Math.round((Date.now() - started) / 1000)} s`;

const targets = run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '';
if (process.platform === 'win32' || run(repository, 'worker-build', ['--version']).status !== 0 || !targets.includes('wasm32-unknown-unknown')) {
  if (process.env.SUPERCOV_REQUIRE_RUST_WORKER === '1') {
    console.error('[rust-worker-mutants] worker-build or the wasm32-unknown-unknown target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-worker-mutants] skipped: needs worker-build and the wasm32-unknown-unknown target');
  process.exit(0);
}

const files = {
  'package.json': `{ "name": "grades", "private": true, "devDependencies": { "wrangler": "${WRANGLER}" } }\n`,
  '.gitignore': 'node_modules/\ntarget/\n.wrangler/\nworker/build/\n',
  'Cargo.toml': '[workspace]\nmembers = ["worker", "host"]\nresolver = "2"\n\n[profile.release]\nopt-level = "s"\n',
  'worker/Cargo.toml': '[package]\nname = "grades-worker"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n',
  'worker/wrangler.toml': 'name = "grades-worker"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "./build.sh"\n',
  'worker/build.sh': '#!/bin/sh\n# The Worker, as the project builds it: a dev build, which a gate builds in seconds.\nset -e\nworker-build --dev\n',
  'worker/src/lib.rs': `use worker::*;

pub fn grade(score: u32) -> &'static str {
    if score >= 50 {
        "pass"
    } else {
        "fail"
    }
}

pub fn shout(text: &str) -> String {
    text.to_uppercase()
}

pub fn hidden(score: u32) -> u32 {
    score + 1
}

// The routing is not what the gate is about: its mutants would each start
// the Worker for every test. A cfg never set: no build needs the mutants
// crate, and the attribute still says skip.
#[event(fetch)]
#[cfg_attr(gate_never_set, mutants::skip)]
async fn fetch(request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = request.url()?;
    let score = url
        .query_pairs()
        .find(|(key, _)| key == "score")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    match url.path() {
        "/ready" => Response::ok("ready"),
        "/shout" => Response::ok(shout("hey")),
        "/hidden" => Response::ok(hidden(score).to_string()),
        _ => Response::ok(grade(score)),
    }
}
`,
  'host/Cargo.toml': '[package]\nname = "host"\nversion = "0.1.0"\nedition = "2021"\n',
  'host/src/lib.rs': '//! The suite that starts the Worker.\n',
  'host/tests/worker.rs': `use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// One Worker at a time: a \`wrangler dev\` may build the crate in place.
fn exclusive() -> std::fs::File {
    let file = std::fs::File::create(std::env::temp_dir().join("grades-worker-mutants.lock")).unwrap();
    file.lock().unwrap();
    file
}

struct Dev {
    child: Child,
    port: u16,
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", "--", &format!("-{}", self.child.id())]).status();
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

fn serve_with(wrangler: &str) -> Dev {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new(wrangler)
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../worker"))
        .args(["dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("wrangler");
    let dev = Dev { child, port };
    // Two minutes: a Worker that never answers fails its test, not the job.
    for _ in 0..240 {
        if get(port, "/ready").as_deref() == Some("ready") {
            return dev;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("wrangler dev never answered");
}

#[test]
fn a_high_score_passes() {
    let _one = exclusive();
    let dev = serve_with("wrangler");
    assert_eq!(get(dev.port, "/?score=70").as_deref(), Some("pass"));
}

#[test]
fn a_low_score_fails() {
    let _one = exclusive();
    let dev = serve_with("wrangler");
    assert_eq!(get(dev.port, "/?score=10").as_deref(), Some("fail"));
}

/// Reaches \`shout\`, and checks only that the Worker answered.
#[test]
fn the_worker_shouts() {
    let _one = exclusive();
    let dev = serve_with("wrangler");
    assert!(get(dev.port, "/shout").is_some());
}

/// Reaches \`hidden\`. During a strong pass it starts the real wrangler by
/// its own path, past Supercov's: a Worker built apart from the pass.
#[test]
fn the_hidden_route_answers() {
    let _one = exclusive();
    let wrangler = match std::env::var_os("SUPERCOV_STRONG_RESULTS") {
        Some(_) => std::env::var("GATE_REAL_WRANGLER").expect("the gate names the real wrangler"),
        None => "wrangler".to_owned(),
    };
    let dev = serve_with(&wrangler);
    assert!(get(dev.port, "/hidden?score=1").is_some());
}
`,
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  chmodSync(resolve(root, 'worker/build.sh'), 0o755);
  const installed = run(root, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(run(root, 'cargo', ['generate-lockfile', '-q']).status, 0);
  assert.equal(run(root, 'git', ['init', '-q']).status, 0);
  assert.equal(run(root, 'git', ['add', '-A']).status, 0);
  assert.equal(run(root, 'git', ['-c', 'user.name=gate', '-c', 'user.email=gate@example.com', 'commit', '-qm', 'fixture']).status, 0);

  const bin = resolve(root, 'node_modules/.bin');
  const path = `${bin}${delimiter}${process.env.PATH}`;
  const env = { PATH: path, GATE_REAL_WRANGLER: realpathSync(resolve(bin, 'wrangler')) };
  const measured = run(root, binary, ['--', 'cargo', 'test'], env, LIMIT);
  console.log(`[rust-worker-mutants] measured, ${took()}`);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  assert.doesNotMatch(measured.stderr, /not measured: [^\n]*Rust Worker/u, measured.stderr);

  const pass = run(root, binary, ['runs', 'latest', 'mutants', 'test', '--json'], { ...env, SUPERCOV_PROGRESS_INTERVAL_MS: '20000' }, LIMIT);
  console.log(`[rust-worker-mutants] strong pass, ${took()}`);
  console.log(pass.stderr.split('\n').filter((line) => line.startsWith('[supercov]')).join('\n'));
  assert.equal(pass.status, 0, `${pass.stdout}\n${pass.stderr}`);
  const listed = JSON.parse(run(root, binary, ['runs', 'latest', 'mutants', '--json', '--limit', '0']).stdout).data.mutants;
  const of = (line) => listed.filter((mutant) => mutant.file === 'worker/src/lib.rs' && mutant.span.start[0] === line);
  const said = (mutants) => mutants.map((mutant) => `${mutant.id}: ${JSON.stringify(mutant.strong)}`).join('\n');

  // grade()'s comparison, made `<`: killed.
  const comparison = of(4).find((mutant) => mutant.mutator === 'BinaryOperator' && mutant.replacement === '<');
  assert.ok(comparison, `no \`<\` mutant of grade()'s comparison\n${said(of(4))}\n${pass.stderr}`);
  assert.equal(comparison.strong?.status, 'killed', `${said([comparison])}\n${pass.stderr}`);

  // shout(), reached and unchecked: survived, having run.
  const shouted = of(12);
  assert.ok(shouted.length > 0, `no mutant of shout()\n${pass.stderr}`);
  for (const mutant of shouted) {
    assert.equal(mutant.strong?.status, 'survived', `${said([mutant])}\n${pass.stderr}`);
  }

  // hidden(), reached only by a Worker built apart from the pass: not
  // reached in its mutated form, not survived.
  const hidden = of(16);
  assert.ok(hidden.length > 0, `no mutant of hidden()\n${pass.stderr}`);
  for (const mutant of hidden) {
    assert.equal(mutant.strong?.status, 'no-coverage', `${said([mutant])}\n${pass.stderr}`);
    assert.match(mutant.strong?.reason ?? '', /^not-reached-mutated: /u, said([mutant]));
  }

  // The project is as it was.
  const status = run(root, 'git', ['status', '--porcelain', '--untracked-files=all']).stdout;
  assert.equal(status.trim(), '', status);
  console.log(`[rust-worker-mutants] a Worker built by ./build.sh under wrangler dev: grade()'s comparison killed, shout()'s ${shouted.length} survived having run, hidden()'s ${hidden.length} not reached in mutated form`);
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-worker-mutants-')))
    rmSync(root, { recursive: true, force: true });
}
