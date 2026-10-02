// `supercov runs <run> assertions` against a local stand-in for Jev: every
// question answered by rule (a console.log or print line is not asserted;
// everything else is), so the verdicts, the answer cache and --dry-run are
// checked without the network, for JavaScript, Python (unittest, so only an
// interpreter is needed), Go, Ruby (Minitest and test-unit), Rust (a doctest
// among its tests), Java (Maven, offline), and Vitest and Jest, linked from
// the repository's own node_modules.
// --javascript-only
// skips the others where they are not installed, as in the Alpine image.
import { spawn, spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync, existsSync } from "node:fs";
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

// Ruby 3.3 or newer measures each test exactly; an older one is skipped.
const rubyCandidates = process.env.SUPERCOV_RUBY ? [process.env.SUPERCOV_RUBY] : ["/opt/homebrew/opt/ruby/bin/ruby", "ruby"];
const ruby = rubyCandidates.find((program) => {
  const version = spawnSync(program, ["-e", "print RUBY_VERSION"], { encoding: "utf8" }).stdout ?? "";
  const [major, minor] = version.split(".").map(Number);
  return major > 3 || (major === 3 && minor >= 3);
});
const rb = join(scratch, "rb");
write = writer(rb);
write("lib/cart.rb", `module Cart
  def self.total(items)
    sum = 0
    items.each do |item|
      sum += item[:price] * item[:quantity]
    end
    if sum > 100
      sum = sum * 9 / 10
    end
    puts "total #{sum}"
    sum
  end
end
`);
write("lib/format.rb", `module Format
  def self.label(name)
    trimmed = name.strip
    if trimmed.empty?
      return "unknown"
    end
    trimmed.upcase
  end
end
`);
write("test/cart_test.rb", `require "minitest/autorun"
require "cart"

class CartTest < Minitest::Test
  def test_adds_prices
    assert_equal 20, Cart.total([{ price: 10, quantity: 2 }])
  end

  def test_discounts_big_carts
    assert_equal 108, Cart.total([{ price: 60, quantity: 2 }])
  end
end
`);
write("test/format_test.rb", `require "minitest/autorun"
require "format"

class FormatTest < Minitest::Test
  def test_upper_cases_a_name
    assert_equal "AB", Format.label("  ab ")
  end

  def test_names_a_blank_label
    assert_equal "unknown", Format.label("  ")
  end
end
`);

const rs = join(scratch, "rs");
write = writer(rs);
write("Cargo.toml", '[package]\nname = "shop"\nversion = "0.0.0"\nedition = "2021"\n');
write("src/lib.rs", "//! ```\n//! assert_eq!(shop::cart::total(&[]), 0);\n//! ```\npub mod cart;\npub mod format;\n");
write("src/cart.rs", `pub struct Item {
    pub price: u32,
    pub quantity: u32,
}

pub fn total(items: &[Item]) -> u32 {
    let mut sum = 0;
    for item in items {
        sum += item.price * item.quantity;
    }
    if sum > 100 {
        sum = sum * 9 / 10;
    }
    println!("total {sum}");
    sum
}
`);
write("src/format.rs", `pub fn label(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    trimmed.to_uppercase()
}
`);
write("tests/cart.rs", `use shop::cart::{total, Item};

#[test]
fn adds_prices() {
    assert_eq!(total(&[Item { price: 10, quantity: 2 }]), 20);
}

#[test]
fn discounts_big_carts() {
    assert_eq!(total(&[Item { price: 60, quantity: 2 }]), 108);
}
`);
write("tests/format.rs", `use shop::format::label;

#[test]
fn upper_cases_a_name() {
    assert_eq!(label("  ab "), "AB");
}

#[test]
fn names_a_blank_label() {
    assert_eq!(label("  "), "unknown");
}
`);
const hasCargo = spawnSync("cargo", ["--version"]).status === 0;

const jv = join(scratch, "java");
write = writer(jv);
write("pom.xml", `<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>shop</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
      </plugin>
    </plugins>
  </build>
</project>
`);
write("src/main/java/shop/Cart.java", `package shop;

public final class Cart {
    public record Item(int price, int quantity) {}

    public static int total(Item[] items) {
        int sum = 0;
        for (Item item : items) {
            sum += item.price() * item.quantity();
        }
        if (sum > 100) {
            sum = sum * 9 / 10;
        }
        System.out.println("total " + sum);
        return sum;
    }
}
`);
write("src/main/java/shop/Format.java", `package shop;

public final class Format {
    public static String label(String name) {
        String trimmed = name.strip();
        if (trimmed.isEmpty()) {
            return "unknown";
        }
        return trimmed.toUpperCase();
    }
}
`);
write("src/test/java/shop/CartTest.java", `package shop;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class CartTest {
    @Test
    void addsPrices() {
        assertEquals(20, Cart.total(new Cart.Item[] {new Cart.Item(10, 2)}));
    }

    @Test
    void discountsBigCarts() {
        assertEquals(108, Cart.total(new Cart.Item[] {new Cart.Item(60, 2)}));
    }
}
`);
write("src/test/java/shop/FormatTest.java", `package shop;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class FormatTest {
    @Test
    void upperCasesAName() {
        assertEquals("AB", Format.label("  ab "));
    }

    @Test
    void namesABlankLabel() {
        assertEquals("unknown", Format.label("  "));
    }
}
`);
// Maven resolves JUnit from the network on a cold cache; without either the
// scenario is skipped.
const hasMaven =
  spawnSync("mvn", ["-q", "-o", "test"], { cwd: jv, encoding: "utf8" }).status === 0 &&
  (rmSync(join(jv, "target"), { recursive: true, force: true }), true);

// The JavaScript sources again, under Vitest and Jest.
const repositoryRoot = resolve(import.meta.dirname, "..");
function jsRunner(name, runner, imports) {
  const root = join(scratch, name);
  mkdirSync(join(root, "node_modules", ".bin"), { recursive: true });
  cpSync(join(js, "src"), join(root, "src"), { recursive: true });
  symlinkSync(join(repositoryRoot, "node_modules", runner), join(root, "node_modules", runner));
  symlinkSync(join(repositoryRoot, "node_modules", ".bin", runner), join(root, "node_modules", ".bin", runner));
  if (runner === "vitest") symlinkSync(join(repositoryRoot, "node_modules", "vite"), join(root, "node_modules", "vite"));
  write = writer(root);
  write("package.json", JSON.stringify({ name: `assertions-${name}`, private: true, type: "module", scripts: { test: runner === "vitest" ? "vitest run" : "jest" } }));
  if (runner === "jest") write("jest.config.mjs", "export default { transform: {}, testMatch: ['**/tests/**/*.test.mjs'] };\n");
  write("tests/cart.test.mjs", `${imports}
import { total } from '../src/cart.mjs'

test('adds prices', () => {
  expect(total([{ price: 10, quantity: 2 }])).toBe(20)
})

test('discounts big carts', () => {
  expect(total([{ price: 60, quantity: 2 }])).toBe(108)
})
`);
  write("tests/format.test.mjs", `${imports}
import { label } from '../src/format.mjs'

test('upper-cases a name', () => {
  expect(label('  ab ')).toBe('AB')
})

test('names a blank label', () => {
  expect(label('  ')).toBe('unknown')
})
`);
  return root;
}
const hasRunner = (runner) => existsSync(join(repositoryRoot, "node_modules", runner));
const vitestRoot = hasRunner("vitest") && jsRunner("vitest", "vitest", "import { test, expect } from 'vitest'");
const jestRoot = hasRunner("jest") && jsRunner("jest", "jest", "import { test, expect } from '@jest/globals'");

// Ruby's test-unit, where it is installed: the Minitest suite rewritten.
const tu = join(scratch, "test-unit");
const testUnit =
  // `gem` only asks whether it is installed; `require` would run an empty
  // suite at exit, which fails.
  !javascriptOnly && ruby && spawnSync(ruby, ["-e", "gem 'test-unit'"], { encoding: "utf8" }).status === 0;
if (testUnit) {
  cpSync(join(rb, "lib"), join(tu, "lib"), { recursive: true });
  write = writer(tu);
  for (const file of ["test/cart_test.rb", "test/format_test.rb"])
    write(file, readFileSync(join(rb, file), "utf8").replace('require "minitest/autorun"', 'require "test/unit"').replace("Minitest::Test", "Test::Unit::TestCase"));
}

// pytest, installed into a virtual environment of its own; a conftest
// fixture supplies one test's cart.
const pt = join(scratch, "pytest");
const venv = join(scratch, "venv");
const venvPython = join(venv, process.platform === "win32" ? "Scripts/python.exe" : "bin/python");
// The newest interpreter, as the Python gate picks it: an older pip may not
// reach the index at all.
const venvBase = ["python3.14", "python3.13", "python3.12", "python3.11", python].find(
  (candidate) => spawnSync(candidate, ["--version"]).status === 0,
);
const hasPytest =
  !javascriptOnly &&
  spawnSync(venvBase, ["-m", "venv", venv]).status === 0 &&
  spawnSync(venvPython, ["-m", "pip", "install", "--quiet", "--disable-pip-version-check", "pytest"], { encoding: "utf8" }).status === 0;
if (hasPytest) {
  cpSync(join(py, "src"), join(pt, "src"), { recursive: true });
  write = writer(pt);
  write("tests/__init__.py", "");
  write("tests/conftest.py", `import pytest


@pytest.fixture
def big_cart():
    return [{"price": 60, "quantity": 2}]
`);
  write("tests/test_cart.py", `from src.cart import total


def test_adds_prices():
    assert total([{"price": 10, "quantity": 2}]) == 20


def test_discounts_big_carts(big_cart):
    assert total(big_cart) == 108
`);
  write("tests/test_format.py", `from src.format import label


def test_upper_cases_a_name():
    assert label("  ab ") == "AB"


def test_names_a_blank_label():
    assert label("  ") == "unknown"
`);
}

// RSpec, installed into a gem directory of its own.
const rs2 = join(scratch, "rspec");
const gems = join(scratch, "gems");
const gemProgram = ruby && join(resolve(ruby, ".."), process.platform === "win32" ? "gem.cmd" : "gem");
const hasRspec =
  !javascriptOnly && ruby &&
  spawnSync(gemProgram, ["install", "--install-dir", gems, "--no-document", "--quiet", "rspec"], { encoding: "utf8" }).status === 0;
if (hasRspec) {
  cpSync(join(rb, "lib"), join(rs2, "lib"), { recursive: true });
  write = writer(rs2);
  write(".rspec", "--require spec_helper\n");
  write("spec/spec_helper.rb", "$LOAD_PATH.unshift File.expand_path('../lib', __dir__)\n");
  write("spec/cart_spec.rb", `require "cart"

RSpec.describe Cart do
  it "adds prices" do
    expect(Cart.total([{ price: 10, quantity: 2 }])).to eq(20)
  end

  it "discounts big carts" do
    expect(Cart.total([{ price: 60, quantity: 2 }])).to eq(108)
  end
end
`);
  write("spec/format_spec.rb", `require "format"

RSpec.describe Format do
  it "upper-cases a name" do
    expect(Format.label("  ab ")).to eq("AB")
  end

  it "names a blank label" do
    expect(Format.label("  ")).to eq("unknown")
  end
end
`);
}

// TestNG through Maven, offline, from the Java sources.
const tng = join(scratch, "testng");
if (hasMaven) {
  cpSync(join(jv, "src/main"), join(tng, "src/main"), { recursive: true });
  write = writer(tng);
  write("pom.xml", readFileSync(join(jv, "pom.xml"), "utf8")
    .replace("<groupId>org.junit.jupiter</groupId>\n      <artifactId>junit-jupiter</artifactId>\n      <version>5.10.2</version>",
      "<groupId>org.testng</groupId>\n      <artifactId>testng</artifactId>\n      <version>7.10.2</version>"));
  for (const [name, body] of [
    ["CartTest", `    @Test
    public void addsPrices() {
        assertEquals(Cart.total(new Cart.Item[] {new Cart.Item(10, 2)}), 20);
    }

    @Test
    public void discountsBigCarts() {
        assertEquals(Cart.total(new Cart.Item[] {new Cart.Item(60, 2)}), 108);
    }`],
    ["FormatTest", `    @Test
    public void upperCasesAName() {
        assertEquals(Format.label("  ab "), "AB");
    }

    @Test
    public void namesABlankLabel() {
        assertEquals(Format.label("  "), "unknown");
    }`],
  ])
    write(`src/test/java/shop/${name}.java`, `package shop;

import static org.testng.Assert.assertEquals;

import org.testng.annotations.Test;

public class ${name} {
${body}
}
`);
}
const hasTestng =
  hasMaven && spawnSync("mvn", ["-q", "-o", "test"], { cwd: tng, encoding: "utf8" }).status === 0 &&
  (rmSync(join(tng, "target"), { recursive: true, force: true }), true);

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
      if (!request.state?.code_run || !request.state?.test_code || !request.state?.test?.name) { console.error("[stand-in] incomplete state for " + request.state?.test?.name + ": " + Object.keys(request.state ?? {}).filter((k) => !request.state[k]).join(",")); res.writeHead(400).end(); return; }
      // The test's own code, found by its title, not the file's head.
      const title = request.state.test.name.split(/::| > /).pop().split("[")[0].split(".").pop().split("#").pop();
      // A doctest and an RSpec example are named by position, not by a title
      // their code contains.
      const positional = /\\(line \\d+\\)|\\[\\d+(:\\d+)*\\]$/.test(request.state.test.name);
      if (!positional && !request.state.test_code.includes(title)) { console.error("[stand-in] " + request.state.test.name + ": test code without " + title); res.writeHead(400).end(); return; }
      appendFileSync(${JSON.stringify(counter)}, Object.keys(request.questions).length + "\\n");
      const answers = Object.fromEntries(Object.entries(request.questions).map(([id, q]) =>
        [id, { type: "noul", noul: /console\\.log|print\\(|Println\\(|println!\\(|puts |System\\.out\\.println\\(/.test(q.instructions.task) ? 0.1 : 0.9 }]));
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
function scenario({ name, root, command, logFile, logText, logLine, edit, statements = 11, runEnv = binary, upper = /upper/i }) {
  const asserted = statements - 1;
  const share = Math.round((asserted / statements) * 1000) / 10;
  const assess = (...extra) => {
    const result = requireSupercov(root, ["runs", latestRun(root), "assertions", "assess", "--json", ...extra], { env });
    const envelope = JSON.parse(result.stdout);
    if (envelope.ok !== true) throw new Error(`assertions failed: ${result.stdout}`);
    return envelope.data;
  };
  requireSupercov(root, ["--", ...command], { env: runEnv });

  const start = requests();
  const dry = assess("--dry-run");
  if (dry.statements !== statements || requests() !== start) fail(`${name}: a dry run estimates ${statements} statements and sends nothing`, { dry, requests: requests() });

  const first = assess();
  const s = first.summary;
  if (s.statements !== statements || s.asserted !== asserted || s.notAsserted !== 1 || s.requests === 0)
    fail(`${name}: expected ${asserted} of ${statements} statements asserted after requests`, first);
  // The overview names the file with the statement not asserted; the file's
  // page names the statement.
  if (first.files?.[0]?.file !== logFile || first.files[0].notAsserted !== 1)
    fail(`${name}: expected ${logFile} first among the files, with one statement not asserted`, first);
  if (!existsSync(resolve(root, ".supercov/runs", latestRun(root), "assertion-coverage.json")))
    fail(`${name}: expected the run's saved result`, first);

  // Reading needs neither the network nor a key.
  const offline = { env: { ...binary, TYPESAFE_API_KEY: "", TYPESAFE_BASE_URL: "http://127.0.0.1:9" } };
  const read = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", logFile, "--json"], offline).stdout).data;
  const not = read.notAsserted?.[0];
  if (read.summary?.asserted !== asserted || not?.line !== logLine || !not.text.includes(logText) || not.change !== "skipped")
    fail(`${name}: reading the file returns its statement not asserted`, read);
  // One statement shows every asked test's answer.
  const statement = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", `${logFile}:${logLine}`, "--json"], offline).stdout).data;
  if (statement.statements?.[0]?.asserted !== false || !statement.statements[0].answers?.length || statement.statements[0].answers.some((answer) => answer.catches))
    fail(`${name}: the statement view shows each test's answer, none catching it`, statement);

  const sent = requests();
  const again = assess();
  if (again.summary.requests !== 0 || requests() !== sent || again.summary.asserted !== asserted)
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
    if (narrowed.length !== 1 || !upper.test(narrowed[0]))
      fail(`${name}: ${flag} keeps only the test that runs the edited line`, narrowed);
  }

  requireSupercov(root, ["--", ...command], { env: runEnv });
  const unassessed = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", "--json"], { env }).stdout).data;
  if (unassessed.assessed !== false || unassessed.lastAssessed?.percentage !== share)
    fail(`${name}: a new run reads as not assessed and names the last assessment`, unassessed);
  const before = requests();
  const changed = assess();
  const asked = requests() - before;
  if (changed.summary.statements !== statements || changed.summary.requests === 0 || changed.summary.answersReused < statements - 4 || asked > 2)
    fail(`${name}: after editing ${file} only its questions are asked again (${asked} requests)`, changed);
  console.log(`[assertions] ${name}: ${asserted} of ${statements} asserted, the ${logText} line not; a repeat pass sent nothing; tests affected kept the one test that runs the edit; editing ${file} sent ${asked} requests and reused ${changed.summary.answersReused} answers`);
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
  if (!javascriptOnly && ruby) {
    const bin = resolve(ruby, "..");
    const runEnv = { ...binary, PATH: `${bin}:${process.env.PATH}`, RUBYOPT: "" };
    scenario({ name: "Ruby", root: rb, runEnv,
      command: [ruby, "-Ilib", "-Itest", "-e", 'Dir.glob("test/*_test.rb").sort.each { |f| require File.expand_path(f) }'],
      logFile: "lib/cart.rb", logText: "puts", logLine: 10,
      edit: ["lib/format.rb", "trimmed.upcase", "trimmed.upcase.strip"] });
  } else if (!javascriptOnly) console.log("[assertions] Ruby: skipped, no Ruby 3.3 or newer");
  if (!javascriptOnly && hasCargo)
    scenario({ name: "Rust", root: rs, command: ["cargo", "test", "--quiet"], logFile: "src/cart.rs", logText: "println!", logLine: 14,
      edit: ["src/format.rs", "trimmed.to_uppercase()", "trimmed.to_uppercase().trim().to_string()"] });
  else if (!javascriptOnly) console.log("[assertions] Rust: skipped, cargo is not on PATH");
  if (!javascriptOnly && hasMaven)
    // The JVM frontend counts an `if` as a branch, not a statement.
    scenario({ name: "Java", root: jv, statements: 8, command: ["mvn", "-q", "-o", "test"], logFile: "src/main/java/shop/Cart.java", logText: "System.out.println", logLine: 14,
      edit: ["src/main/java/shop/Format.java", "trimmed.toUpperCase()", "trimmed.toUpperCase().strip()"] });
  else if (!javascriptOnly) console.log("[assertions] Java: skipped, Maven cannot build offline here");
  const jsEdit = ["src/format.mjs", "toUpperCase()", "toLocaleUpperCase()"];
  if (vitestRoot)
    scenario({ name: "Vitest", root: vitestRoot, command: ["npx", "vitest", "run"], logFile: "src/cart.mjs", logText: "console.log", logLine: 9, edit: jsEdit });
  if (jestRoot)
    scenario({ name: "Jest", root: jestRoot, command: ["npx", "jest"], logFile: "src/cart.mjs", logText: "console.log", logLine: 9, edit: jsEdit,
      runEnv: { ...binary, NODE_OPTIONS: "--experimental-vm-modules" } });
  if (hasPytest)
    scenario({ name: "pytest", root: pt, runEnv: { ...binary, PATH: `${join(venv, "bin")}:${process.env.PATH}` },
      command: [venvPython, "-m", "pytest", "-q", "-p", "no:cacheprovider", "tests"], logFile: "src/cart.py", logText: "print", logLine: 7,
      edit: ["src/format.py", "trimmed.upper()", "trimmed.upper().strip()"] });
  else if (!javascriptOnly) console.log("[assertions] pytest: skipped, it could not be installed");
  if (hasRspec) {
    const bin = resolve(ruby, "..");
    scenario({ name: "RSpec", root: rs2, runEnv: { ...binary, GEM_PATH: gems, PATH: `${join(gems, "bin")}:${bin}:${process.env.PATH}`, RUBYOPT: "" },
      // RSpec names an example by its place: the format spec's first.
      command: [join(gems, "bin", "rspec")], logFile: "lib/cart.rb", logText: "puts", logLine: 10, upper: /format_spec\.rb\[1:1\]/,
      edit: ["lib/format.rb", "trimmed.upcase", "trimmed.upcase.strip"] });
  } else if (!javascriptOnly) console.log("[assertions] RSpec: skipped, it could not be installed");
  if (hasTestng)
    scenario({ name: "TestNG", root: tng, statements: 8, command: ["mvn", "-q", "-o", "test"], logFile: "src/main/java/shop/Cart.java", logText: "System.out.println", logLine: 14,
      edit: ["src/main/java/shop/Format.java", "trimmed.toUpperCase()", "trimmed.toUpperCase().strip()"] });
  else if (!javascriptOnly) console.log("[assertions] TestNG: skipped, Maven cannot build it offline here");
  if (testUnit) {
    const bin = resolve(ruby, "..");
    scenario({ name: "test-unit", root: tu, runEnv: { ...binary, PATH: `${bin}:${process.env.PATH}`, RUBYOPT: "" },
      command: [ruby, "-Ilib", "-Itest", "-e", 'Dir.glob("test/*_test.rb").sort.each { |f| require File.expand_path(f) }'],
      logFile: "lib/cart.rb", logText: "puts", logLine: 10,
      edit: ["lib/format.rb", "trimmed.upcase", "trimmed.upcase.strip"] });
  }
} finally {
  server.kill();
  rmSync(scratch, { recursive: true, force: true });
}
