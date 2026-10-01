// `supercov runs <run> assertions` against a local stand-in for Jev: every
// question answered by rule (a console.log or print line is not asserted;
// everything else is), so the verdicts, the answer cache and --dry-run are
// checked without the network, for JavaScript, Python (unittest, so only an
// interpreter is needed) and Go. --javascript-only skips the others where
// they are not installed, as in the Alpine image.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { latestRun, requireSupercov } from "./coverage-test-helpers.mjs";

// An optional binary, as the Alpine job passes its release musl build.
const args = process.argv.slice(2);
const javascriptOnly = args.includes("--javascript-only");
const binaryPath = args.find((a) => !a.startsWith("--"));
const binary = binaryPath ? { SUPERCOV_RUST_BINARY: resolve(binaryPath) } : {};

const scratch = mkdtempSync(join(tmpdir(), "supercov-assertions-"));
const js = join(scratch, "js");
const py = join(scratch, "py");
const writer = (root) => (file, text) => {
  mkdirSync(resolve(root, file, ".."), { recursive: true });
  writeFileSync(resolve(root, file), text);
};
let write = writer(js);
write("package.json", JSON.stringify({ name: "assertions-fixture", private: true, type: "module", scripts: { test: "node --test" } }));
write("src/cart.mjs", `export function total(items) {
  let sum = 0
  for (const item of items) {
    sum += item.price * item.quantity
  }
  if (sum > 100) {
    sum = sum * 0.9
  }
  console.log('total', sum)
  return sum
}
`);
write("src/format.mjs", `export function label(name) {
  const trimmed = name.trim()
  if (!trimmed) {
    return 'unknown'
  }
  return trimmed.toUpperCase()
}
`);
write("tests/cart.test.mjs", `import { test } from 'node:test'
import assert from 'node:assert/strict'
import { total } from '../src/cart.mjs'

test('adds prices', () => {
  assert.equal(total([{ price: 10, quantity: 2 }]), 20)
})

test('discounts big carts', () => {
  assert.equal(total([{ price: 60, quantity: 2 }]), 108)
})
`);
write("tests/format.test.mjs", `import { test } from 'node:test'
import assert from 'node:assert/strict'
import { label } from '../src/format.mjs'

test('upper-cases a name', () => {
  assert.equal(label('  ab '), 'AB')
})

test('names a blank label', () => {
  assert.equal(label('  '), 'unknown')
})
`);

write = writer(py);
write("src/__init__.py", "");
write("src/cart.py", `def total(items):
    total = 0
    for item in items:
        total += item["price"] * item["quantity"]
    if total > 100:
        total = total * 0.9
    print("total", total)
    return total
`);
write("src/format.py", `def label(name):
    trimmed = name.strip()
    if not trimmed:
        return "unknown"
    return trimmed.upper()
`);
write("tests/__init__.py", "");
write("tests/test_cart.py", `import unittest
from src.cart import total


class CartTest(unittest.TestCase):
    def test_adds_prices(self):
        self.assertEqual(total([{"price": 10, "quantity": 2}]), 20)

    def test_discounts_big_carts(self):
        self.assertEqual(total([{"price": 60, "quantity": 2}]), 108)
`);
write("tests/test_format.py", `import unittest
from src.format import label


class FormatTest(unittest.TestCase):
    def test_upper_cases_a_name(self):
        self.assertEqual(label("  ab "), "AB")

    def test_names_a_blank_label(self):
        self.assertEqual(label("  "), "unknown")
`);
const python = process.platform === "win32" ? "python" : "python3";

const go = join(scratch, "go");
write = writer(go);
write("go.mod", "module example.com/shop\n\ngo 1.21\n");
write("cart.go", `package shop

import "fmt"

type Item struct{ Price, Quantity int }

func Total(items []Item) int {
	sum := 0
	for _, item := range items {
		sum += item.Price * item.Quantity
	}
	if sum > 100 {
		sum = sum * 9 / 10
	}
	fmt.Println("total", sum)
	return sum
}
`);
write("format.go", `package shop

import "strings"

func Label(name string) string {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return "unknown"
	}
	return strings.ToUpper(trimmed)
}
`);
write("cart_test.go", `package shop

import "testing"

func TestAddsPrices(t *testing.T) {
	if got := Total([]Item{{10, 2}}); got != 20 {
		t.Fatal(got)
	}
}

func TestDiscountsBigCarts(t *testing.T) {
	if got := Total([]Item{{60, 2}}); got != 108 {
		t.Fatal(got)
	}
}
`);
write("format_test.go", `package shop

import "testing"

func TestUpperCasesAName(t *testing.T) {
	if got := Label("  ab "); got != "AB" {
		t.Fatal(got)
	}
}

func TestNamesABlankLabel(t *testing.T) {
	if got := Label("  "); got != "unknown" {
		t.Fatal(got)
	}
}
`);
// Go is measured where it is installed; a job that promises it
// (SUPERCOV_REQUIRE_GO) fails without it rather than skipping.
const hasGo = spawnSync("go", ["version"]).status === 0;
if (!hasGo && process.env.SUPERCOV_REQUIRE_GO) throw new Error("SUPERCOV_REQUIRE_GO is set but go is not on PATH");

// The stand-in: answers every noul question, 0.1 for a console.log or print
// line and 0.9 otherwise, and counts requests in a file.
const counter = join(scratch, "requests.txt");
const server = spawn(process.execPath, ["--input-type=module", "-e", `
  import { createServer } from "node:http";
  import { appendFileSync } from "node:fs";
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const request = JSON.parse(body);
      if (req.url !== "/v1/systemone" || req.headers.authorization !== "Bearer test-key") { res.writeHead(401).end(); return; }
      if (!request.state?.code_run || !request.state?.test_code || !request.state?.test?.name) { res.writeHead(400).end(); return; }
      // The test's own code, found by its title, not the file's head.
      const title = request.state.test.name.split(/::| > /).pop().split("[")[0].split(".").pop();
      if (!request.state.test_code.includes(title)) { res.writeHead(400).end(); return; }
      appendFileSync(${JSON.stringify(counter)}, Object.keys(request.questions).length + "\\n");
      const answers = Object.fromEntries(Object.entries(request.questions).map(([id, q]) =>
        [id, { type: "noul", noul: /console\\.log|print\\(|Println\\(/.test(q.instructions.task) ? 0.1 : 0.9 }]));
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ model: request.model, answers, usage: { input_tokens: Math.ceil(body.length / 4), output_tokens: 1 } }));
    });
  });
  server.listen(0, "127.0.0.1", () => console.log(server.address().port));
`], { stdio: ["ignore", "pipe", "inherit"] });
const port = await new Promise((ok) => server.stdout.once("data", (d) => ok(Number(String(d).trim()))));
const env = { ...binary, TYPESAFE_BASE_URL: `http://127.0.0.1:${port}`, TYPESAFE_API_KEY: "test-key" };
const requests = () => (existsSync(counter) ? readFileSync(counter, "utf8").trim().split("\n").filter(Boolean).length : 0);
const fail = (message, data) => {
  server.kill();
  throw new Error(`${message}: ${JSON.stringify(data, null, 1)}`);
};

// The same checks on each language: 10 of 11 asserted, the logging line not;
// reading offline; a repeat pass sending nothing; one edited line asked again.
function scenario({ name, root, command, logFile, logText, logLine, edit }) {
  const assess = (...extra) => {
    const result = requireSupercov(root, ["runs", latestRun(root), "assertions", "assess", "--json", ...extra], { env });
    const envelope = JSON.parse(result.stdout);
    if (envelope.ok !== true) throw new Error(`assertions failed: ${result.stdout}`);
    return envelope.data;
  };
  requireSupercov(root, ["--", ...command], { env: binary });

  const start = requests();
  const dry = assess("--dry-run");
  if (dry.statements !== 11 || requests() !== start) fail(`${name}: a dry run estimates 11 statements and sends nothing`, { dry, requests: requests() });

  const first = assess();
  const s = first.summary;
  if (s.statements !== 11 || s.asserted !== 10 || s.notAsserted !== 1 || s.requests === 0)
    fail(`${name}: expected 10 of 11 statements asserted after requests`, first);
  const not = first.notAsserted[0];
  if (not.file !== logFile || !not.text.includes(logText) || not.change !== "skipped")
    fail(`${name}: expected the ${logText} line as the one not asserted`, first);
  if (!existsSync(resolve(root, ".supercov/runs", latestRun(root), "assertion-coverage.json")))
    fail(`${name}: expected the run's saved result`, first);

  // Reading needs neither the network nor a key.
  const read = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", "--json"], { env: { ...binary, TYPESAFE_API_KEY: "", TYPESAFE_BASE_URL: "http://127.0.0.1:9" } }).stdout).data;
  if (read.summary?.asserted !== 10 || read.notAsserted?.[0]?.line !== logLine) fail(`${name}: reading returns the saved result`, read);

  const sent = requests();
  const again = assess();
  if (again.summary.requests !== 0 || requests() !== sent || again.summary.asserted !== 10)
    fail(`${name}: a second pass over the same run reuses every answer`, again);

  // One line of the format module changes: its statements are asked again,
  // the cart module's answers are reused.
  const [file, from, to] = edit;
  writeFileSync(resolve(root, file), readFileSync(resolve(root, file), "utf8").replace(from, to));

  // Test impact against the assessed run, before the tests run again: the
  // edited return is run by one test (the blank label returns early), and
  // once `assess --changed` has asked every test that ran it, --asserting
  // keeps exactly that one.
  const run = latestRun(root);
  const affected = () => JSON.parse(requireSupercov(root, ["runs", run, "tests", "affected", "--json"], { env }).stdout).data;
  const coverageOnly = affected();
  if (coverageOnly.assertions?.available !== true || coverageOnly.affected.length !== 2)
    fail(`${name}: tests affected reads the assessment and lists both format tests`, coverageOnly);
  requireSupercov(root, ["runs", run, "assertions", "assess", "--changed"], { env });
  const impact = affected();
  const verdicts = impact.affected.map((t) => t.assertion.verdict).sort();
  if (impact.assertions.impact !== true || JSON.stringify(verdicts) !== JSON.stringify(["catches", "misses"]))
    fail(`${name}: after assess --changed one format test catches the edit and the other missed it`, impact);
  for (const flag of ["--ran-changed", "--asserting"]) {
    const narrowed = requireSupercov(root, ["runs", run, "tests", "affected", "--names", flag], { env }).stdout.trim().split("\n");
    if (narrowed.length !== 1 || !/upper/i.test(narrowed[0]))
      fail(`${name}: ${flag} keeps only the test that runs the edited line`, narrowed);
  }

  requireSupercov(root, ["--", ...command], { env: binary });
  const unassessed = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", "--json"], { env }).stdout).data;
  if (unassessed.assessed !== false || unassessed.lastAssessed?.percentage !== 90.9)
    fail(`${name}: a new run reads as not assessed and names the last assessment`, unassessed);
  const before = requests();
  const changed = assess();
  const asked = requests() - before;
  if (changed.summary.statements !== 11 || changed.summary.requests === 0 || changed.summary.answersReused < 7 || asked > 2)
    fail(`${name}: after editing ${file} only its questions are asked again (${asked} requests)`, changed);
  console.log(`[assertions] ${name}: 10 of 11 asserted, the ${logText} line not; a repeat pass sent nothing; tests affected kept the one test that runs the edit; editing ${file} sent ${asked} requests and reused ${changed.summary.answersReused} answers`);
}

try {
  scenario({ name: "JavaScript", root: js, command: ["npm", "test"], logFile: "src/cart.mjs", logText: "console.log", logLine: 9,
    edit: ["src/format.mjs", "toUpperCase()", "toLocaleUpperCase()"] });
  if (!javascriptOnly)
    scenario({ name: "Python", root: py, command: [python, "-m", "unittest", "discover", "-s", "tests", "-t", "."], logFile: "src/cart.py", logText: "print", logLine: 7,
      edit: ["src/format.py", "trimmed.upper()", "trimmed.upper().strip()"] });
  if (!javascriptOnly && hasGo)
    scenario({ name: "Go", root: go, command: ["go", "test", "./..."], logFile: "cart.go", logText: "Println", logLine: 15,
      edit: ["format.go", "strings.ToUpper(trimmed)", "strings.ToUpper(strings.TrimSpace(trimmed))"] });
  else if (!javascriptOnly) console.log("[assertions] Go: skipped, go is not on PATH");
} finally {
  server.kill();
  rmSync(scratch, { recursive: true, force: true });
}
