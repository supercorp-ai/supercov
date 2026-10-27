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

    /// The cart project in a directory of a repository, committed: one
    /// package of several. The repository comes first; it lasts as long as
    /// the caller holds it.
    pub fn cart_below(label: &str, directory: &str) -> (Self, Self) {
        let repository = Self::empty(label);
        let project = Self {
            root: repository.root.join(directory),
        };
        project.write(
            "package.json",
            r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" } }"#,
        );
        project.write("src/cart.js", CART);
        project.write("test/cart.test.js", CART_TESTS);
        repository.write(".gitignore", ".supercov\n");
        repository.git(&["init", "-q"]);
        repository.commit("cart");
        (repository, project)
    }

    pub fn write(&self, file: &str, contents: &str) {
        let path = self.root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Write an executable script by way of another process. A file this
    /// process has open for writing is inherited by a child another test
    /// thread forks, and held until that child execs; running the script
    /// meanwhile fails with "Text file busy". Written by `cp`, the script
    /// never has a descriptor here.
    #[cfg(unix)]
    pub fn write_script(&self, file: &str, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        let draft = format!("{file}.draft");
        self.write(&draft, contents);
        let copied = std::process::Command::new("cp")
            .arg(self.root.join(&draft))
            .arg(self.root.join(file))
            .status()
            .unwrap();
        assert!(copied.success(), "cp {draft} failed");
        fs::remove_file(self.root.join(&draft)).unwrap();
        fs::set_permissions(self.root.join(file), fs::Permissions::from_mode(0o755)).unwrap();
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
            // How Supercov was started decides the commands it prints.
            "SUPERCOV_PACKAGE_ROOT",
            "SUPERCOV_LAUNCHER",
        ] {
            command.env_remove(name);
        }
        // Under nextest the tests inherit its own variables, which a
        // `cargo nextest run` the test starts would read as its own.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("NEXTEST_") {
                command.env_remove(name);
            }
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
/// status and a body, sent with `retry-after: 0`, or, for the body `wait`,
/// `retry-after: 100000`. The body `cut` is promised whole and sent part.
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
                let wait = if reply == b"wait" { 100_000 } else { 0 };
                if reply == b"cut" {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         content-length: 1000\r\nconnection: close\r\n\r\n{{\"answers\""
                    );
                    return;
                }
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     retry-after: {wait}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
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

/// A reviewer tracing data through the two-file shop in `tests/jev.rs`:
/// `handler` and `route` take caller-supplied data, `findOrders` puts a parameter into
/// a query unprotected, nothing sanitises, and the path between them is
/// confirmed; every other question as `answer_reviewer` answers it.
pub fn answer_taint(id: &str, question: &Value) -> Value {
    let task = question["instructions"]["task"]
        .as_str()
        .unwrap_or_default();
    let function = task.split('`').nth(1).unwrap_or_default();
    let yes = json!({ "type": "noul", "noul": 0.9 });
    let no = json!({ "type": "noul", "noul": 0.1 });
    if id == "path" {
        return yes;
    }
    if let Some((_, label)) = id.split_once('_').filter(|_| id.starts_with('f')) {
        return match label {
            "takes_outside" => {
                if matches!(function, "handler" | "route" | "lookup") {
                    yes
                } else {
                    no
                }
            }
            "reaches_sink" => {
                if function == "findOrders" {
                    yes
                } else {
                    no
                }
            }
            "sanitises" => no,
            "sink_kind" => choose(
                question,
                if function == "findOrders" {
                    "injection_sink"
                } else {
                    "none"
                },
            ),
            _ => answer_reviewer(id, question),
        };
    }
    answer_reviewer(id, question)
}

pub fn choose(question: &Value, preferred: &str) -> Value {
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

/// Supercov on a terminal of its own, started as a shell starts a command:
/// what a test types reaches whichever process the terminal belongs to.
#[cfg(unix)]
pub struct OnTerminal {
    child: std::process::Child,
    ours: fs::File,
    seen: Vec<u8>,
}

#[cfg(unix)]
impl OnTerminal {
    pub fn start(project: &Project, args: &[&str]) -> Self {
        Self::start_with(project, args, &[])
    }

    /// `start`, with these variables set as well.
    pub fn start_with(project: &Project, args: &[&str], env: &[(&str, &str)]) -> Self {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        };
        let (mut ours, mut theirs) = (-1, -1);
        // SAFETY: the two descriptors are owned by nothing else.
        let (ours, theirs) = unsafe {
            assert_eq!(
                libc::openpty(
                    &mut ours,
                    &mut theirs,
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<libc::termios>(),
                    std::ptr::null_mut::<libc::winsize>(),
                ),
                0
            );
            (fs::File::from_raw_fd(ours), fs::File::from_raw_fd(theirs))
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_supercov"));
        command
            .args(args)
            .current_dir(&project.root)
            .stdin(theirs.try_clone().unwrap())
            .stdout(theirs.try_clone().unwrap())
            .stderr(theirs);
        for name in [
            "SUPERCOV_TEST_KIND",
            "SUPERCOV_SOURCE_ROOTS",
            "SUPERCOV_PACKAGE_ROOT",
            "SUPERCOV_LAUNCHER",
        ] {
            command.env_remove(name);
        }
        command.envs(env.iter().copied());
        // SAFETY: both calls are safe between fork and exec. The terminal
        // becomes the new session's, and its keys go to Supercov's group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(command);
        // SAFETY: a flag on a descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(ours.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(ours.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Self {
            child,
            ours,
            seen: Vec::new(),
        }
    }

    fn read_some(&mut self) -> bool {
        let mut buffer = [0_u8; 4096];
        match self.ours.read(&mut buffer) {
            Ok(0) => false,
            Ok(read) => {
                self.seen.extend_from_slice(&buffer[..read]);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(10));
                true
            }
            // Every process that held the other end has gone.
            Err(_) => false,
        }
    }

    pub fn seen(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Waits until the terminal has shown `text`.
    pub fn wait_for(&mut self, text: &str) {
        let started = std::time::Instant::now();
        while !self.seen().contains(text) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60) && self.read_some(),
                "the terminal never showed {text:?}:\n{}",
                self.seen()
            );
        }
    }

    pub fn press(&mut self, keys: &str) {
        self.ours.write_all(keys.as_bytes()).unwrap();
    }

    /// Waits until Supercov itself is stopped, as a shell would see it.
    pub fn wait_until_stopped(&mut self) {
        let started = std::time::Instant::now();
        loop {
            // SAFETY: `info` is plain data the call fills in; asking for
            // stops alone leaves the exit for `finish`.
            let stopped = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut info,
                    libc::WSTOPPED | libc::WNOHANG,
                ) == 0
                    && info.si_code == libc::CLD_STOPPED
            };
            if stopped {
                return;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "Supercov never stopped:\n{}",
                self.seen()
            );
            self.read_some();
        }
    }

    /// Continues Supercov, as `fg` does.
    pub fn resume(&mut self) {
        // SAFETY: a signal to the process this test started.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGCONT) };
    }

    /// The exit status and everything the terminal showed.
    pub fn finish(mut self) -> (i32, String) {
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                while self.read_some() && started.elapsed() < std::time::Duration::from_secs(90) {
                    if self.seen.len() > 4 << 20 {
                        break;
                    }
                    // The last of what was written, then the end of the terminal.
                    let before = self.seen.len();
                    thread::sleep(std::time::Duration::from_millis(30));
                    if !self.read_some() || self.seen.len() == before {
                        break;
                    }
                }
                return (status.code().unwrap_or(-1), self.seen());
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "Supercov never ended:\n{}",
                self.seen()
            );
            self.read_some();
        }
    }
}
