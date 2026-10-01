//! A real project in a git repository, the binary Cargo built for the test,
//! and a local server that answers the way Jev does.
#![allow(dead_code)]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A cart module with three node:test tests that leave some of it unrun.
pub const CART: &str = "\
export function total(items, coupon) {
  let sum = 0;
  for (const item of items) {
    sum += item.price * item.quantity;
  }
  if (coupon && coupon.percent > 0) {
    sum = sum - (sum * coupon.percent) / 100;
  }
  return Math.round(sum * 100) / 100;
}

export function shipping(sum, express) {
  if (express || sum < 50) {
    return express ? 15 : 5;
  }
  return 0;
}

export function describe(items) {
  return items.map((item) => item.name).join(\", \");
}
";

pub const CART_TESTS: &str = "\
import test from \"node:test\";
import assert from \"node:assert/strict\";
import { total, shipping } from \"../src/cart.js\";

test(\"total adds prices\", () => {
  assert.equal(total([{ price: 10, quantity: 2 }]), 20);
});

test(\"total applies a coupon\", () => {
  assert.equal(total([{ price: 10, quantity: 2 }], { percent: 50 }), 10);
});

test(\"shipping is free over fifty\", () => {
  assert.equal(shipping(60, false), 0);
});
";

pub struct Project {
    pub root: PathBuf,
}

impl Project {
    /// An empty directory of its own under the system temporary directory.
    pub fn empty(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-cli-{label}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
        }
    }

    /// The cart project, committed.
    pub fn cart(label: &str) -> Self {
        let project = Self::empty(label);
        project.write(
            "package.json",
            r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" } }"#,
        );
        project.write("src/cart.js", CART);
        project.write("test/cart.test.js", CART_TESTS);
        project.git(&["init", "-q"]);
        project.commit("cart");
        project
    }

    pub fn write(&self, file: &str, contents: &str) {
        let path = self.root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    pub fn read(&self, file: &str) -> String {
        fs::read_to_string(self.root.join(file)).unwrap()
    }

    /// Replaces the one occurrence of `from` in `file`.
    pub fn edit(&self, file: &str, from: &str, to: &str) {
        let contents = self.read(file);
        assert_eq!(contents.matches(from).count(), 1, "{from:?} in {file}");
        self.write(file, &contents.replacen(from, to, 1));
    }

    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn supercov(&self, args: &[&str]) -> Ran {
        self.supercov_with(args, &[])
    }

    pub fn supercov_with(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut command = Command::new(env!("CARGO_BIN_EXE_supercov"));
        command.args(args).current_dir(&self.root);
        for name in [
            "TYPESAFE_API_KEY",
            "TYPESAFE_BASE_URL",
            "TYPESAFE_DEFAULT_MODEL",
            "SUPERCOV_TEST_KIND",
            "SUPERCOV_SOURCE_ROOTS",
        ] {
            command.env_remove(name);
        }
        Ran {
            args: args.iter().map(|arg| arg.to_string()).collect(),
            output: command.envs(env.iter().copied()).output().unwrap(),
        }
    }

    /// Measures `node --test` with these extra arguments and returns the run.
    pub fn measure(&self, extra: &[&str]) -> String {
        let mut args = vec!["--", "node", "--test"];
        args.extend_from_slice(extra);
        self.supercov(&args).succeeds();
        self.latest()
    }

    pub fn latest(&self) -> String {
        self.supercov(&["runs", "--json"]).json()["data"]["runs"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub struct Ran {
    args: Vec<String>,
    pub output: Output,
}

impl Ran {
    pub fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    fn describe(&self) -> String {
        format!(
            "supercov {} exited {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.args.join(" "),
            self.code(),
            self.stdout(),
            self.stderr()
        )
    }

    /// Exits 0; the standard output.
    pub fn succeeds(&self) -> String {
        assert_eq!(self.code(), 0, "{}", self.describe());
        self.stdout()
    }

    /// Exits with `code`; standard output and error together.
    pub fn exits(&self, code: i32) -> String {
        assert_eq!(self.code(), code, "{}", self.describe());
        format!("{}{}", self.stdout(), self.stderr())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.output.stdout)
            .unwrap_or_else(|_| panic!("no JSON report\n{}", self.describe()))
    }
}

/// Asserts each needle appears in `haystack`.
pub fn contains_all(haystack: &str, needles: &[&str]) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "{needle:?} missing from:\n{haystack}"
        );
    }
}

/// What a gateway was sent: the path, the Authorization header and the body.
pub type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

/// Decides one answer: the question's id and the question.
pub type Answerer = fn(&str, &Value) -> Value;

/// Replaces the answer to the request with this number (from 0) with a
/// status and a body, sent with `retry-after: 0`.
pub type Faults = fn(usize) -> Option<(u16, &'static str)>;

/// A local server answering every question as `model`, recording each request.
pub fn gateway(model: &'static str, answerer: Answerer) -> (String, Seen) {
    gateway_with(model, answerer, |_| None)
}

/// `gateway`, with some requests answered by `faults` instead.
pub fn gateway_with(model: &'static str, answerer: Answerer, faults: Faults) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!(
        "http://127.0.0.1:{}/api",
        listener.local_addr().unwrap().port()
    );
    let seen: Seen = Arc::default();
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let record = Arc::clone(&record);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let (mut length, mut authorization) = (0, String::new());
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => authorization = value.trim().to_owned(),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let number = {
                    let mut seen = record.lock().unwrap();
                    seen.push((path, authorization, request.clone()));
                    seen.len() - 1
                };
                let (status, reply) = match faults(number) {
                    Some((status, body)) => (status, body.as_bytes().to_vec()),
                    None => (
                        200,
                        serde_json::to_vec(&answer(&request, model, answerer)).unwrap(),
                    ),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     retry-after: 0\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.len()
                )
                .unwrap();
                stream.write_all(&reply).unwrap();
            });
        }
    });
    (base, seen)
}

fn answer(request: &Value, model: &str, answerer: Answerer) -> Value {
    let mut answers = Map::new();
    for (id, question) in request["questions"].as_object().unwrap() {
        answers.insert(id.clone(), answerer(id, question));
    }
    json!({ "model": model, "answers": answers, "usage": { "input_tokens": 100, "output_tokens": 0 } })
}

/// No for each yes/no, `none` (or the first option) for each choice.
pub fn answer_no(_: &str, question: &Value) -> Value {
    if question["type"] == "choice" {
        choose(question, "none")
    } else {
        json!({ "type": "noul", "noul": 0.1 })
    }
}

/// Yes for each yes/no, the first option other than `none` for each choice.
pub fn answer_yes(_: &str, question: &Value) -> Value {
    if question["type"] == "choice" {
        let first = question["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .find(|option| option.as_str() != "none")
            .cloned()
            .unwrap_or_else(|| "none".into());
        choose(question, &first)
    } else {
        json!({ "type": "noul", "noul": 0.9 })
    }
}

/// Yes to each question but the ones that dismiss a finding (`t0`, `t1`..):
/// what a reviewer says of code that does what it looks like it does.
pub fn answer_findings(id: &str, question: &Value) -> Value {
    let dismisses = id
        .strip_prefix('t')
        .is_some_and(|rest| rest.parse::<usize>().is_ok());
    if dismisses {
        answer_no(id, question)
    } else {
        answer_yes(id, question)
    }
}

/// A reviewer of the shop handler in `tests/jev.rs`: names the weakness a
/// line shows from what is on it, confirms it, and dismisses nothing.
pub fn answer_reviewer(id: &str, question: &Value) -> Value {
    if question["type"] != "choice" || !id.starts_with('n') {
        return answer_findings(id, question);
    }
    let task = question["instructions"]["task"]
        .as_str()
        .unwrap_or_default();
    // "Line 9 of `file.source` reads: `db.query(..)`": the second quoted part.
    let line = task.split('`').nth(3).unwrap_or_default();
    let check = if line.contains(".query(") || line.starts_with("exec(") {
        "injection_sink"
    } else if line.contains("readFile(") {
        "path_from_input"
    } else if line.contains(".send(") {
        "unescaped_output"
    } else {
        "none"
    };
    choose(question, check)
}

fn choose(question: &Value, preferred: &str) -> Value {
    let options: Vec<&String> = question["criteria"].as_object().unwrap().keys().collect();
    let chosen = options
        .iter()
        .find(|option| option.as_str() == preferred)
        .unwrap_or(&options[0]);
    let probabilities: Map<String, Value> = options
        .iter()
        .map(|option| {
            (
                (*option).clone(),
                json!(if option == chosen { 1.0 } else { 0.0 }),
            )
        })
        .collect();
    json!({ "type": "choice", "choice": chosen, "probabilities": probabilities, "confidence": 1.0 })
}

/// The environment that sends Jev's questions to a gateway.
pub fn through(base: &str) -> [(&str, &str); 3] {
    [
        ("TYPESAFE_BASE_URL", base),
        ("TYPESAFE_DEFAULT_MODEL", "typesafe/jev-1.13"),
        ("TYPESAFE_API_KEY", "sk-gateway"),
    ]
}
