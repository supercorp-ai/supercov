// `supercov runs <run> assertions` against a local stand-in for Jev: every
// question answered by rule (a console.log line is not asserted; everything
// else is), so the verdicts, the answer cache and --dry-run are checked
// without the network.
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { latestRun, requireSupercov } from "./coverage-test-helpers.mjs";

// An optional binary, as the Alpine job passes its release musl build.
const binary = process.argv[2] ? { SUPERCOV_RUST_BINARY: resolve(process.argv[2]) } : {};

const root = mkdtempSync(join(tmpdir(), "supercov-assertions-"));
const write = (file, text) => {
  mkdirSync(resolve(root, file, ".."), { recursive: true });
  writeFileSync(resolve(root, file), text);
};
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

// The stand-in: answers every noul question, 0.1 for a console.log line and
// 0.9 otherwise, and counts requests in a file.
const counter = join(root, "requests.txt");
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
      appendFileSync(${JSON.stringify(counter)}, Object.keys(request.questions).length + "\\n");
      const answers = Object.fromEntries(Object.entries(request.questions).map(([id, q]) =>
        [id, { type: "noul", noul: q.instructions.task.includes("console.log") ? 0.1 : 0.9 }]));
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ model: request.model, answers, usage: { input_tokens: Math.ceil(body.length / 4), output_tokens: 1 } }));
    });
  });
  server.listen(0, "127.0.0.1", () => console.log(server.address().port));
`], { stdio: ["ignore", "pipe", "inherit"] });
const port = await new Promise((ok) => server.stdout.once("data", (d) => ok(Number(String(d).trim()))));
const env = { ...binary, TYPESAFE_BASE_URL: `http://127.0.0.1:${port}`, TYPESAFE_API_KEY: "test-key" };
const requests = () => (existsSync(counter) ? readFileSync(counter, "utf8").trim().split("\n").filter(Boolean).length : 0);
const assess = (...extra) => {
  const result = requireSupercov(root, ["runs", latestRun(root), "assertions", "assess", "--json", ...extra], { env });
  const envelope = JSON.parse(result.stdout);
  if (envelope.ok !== true) throw new Error(`assertions failed: ${result.stdout}`);
  return envelope.data;
};
const fail = (message, data) => {
  server.kill();
  throw new Error(`${message}: ${JSON.stringify(data, null, 1)}`);
};

try {
  requireSupercov(root, ["--", "npm", "test"], { env: binary });

  const dry = assess("--dry-run");
  if (dry.statements !== 11 || requests() !== 0) fail("a dry run estimates 11 statements and sends nothing", { dry, requests: requests() });

  const first = assess();
  const s = first.summary;
  if (s.statements !== 11 || s.asserted !== 10 || s.notAsserted !== 1 || s.requests === 0)
    fail("expected 10 of 11 statements asserted after requests", first);
  const not = first.notAsserted[0];
  if (not.file !== "src/cart.mjs" || !not.text.includes("console.log") || not.change !== "skipped")
    fail("expected the console.log line as the one not asserted", first);
  if (!existsSync(resolve(root, ".supercov/runs", latestRun(root), "assertion-coverage.json")))
    fail("expected the run's saved result", first);

  // Reading needs neither the network nor a key.
  const read = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", "--json"], { env: { ...binary, TYPESAFE_API_KEY: "", TYPESAFE_BASE_URL: "http://127.0.0.1:9" } }).stdout).data;
  if (read.summary?.asserted !== 10 || read.notAsserted?.[0]?.line !== 9) fail("reading returns the saved result", read);

  const sent = requests();
  const again = assess();
  if (again.summary.requests !== 0 || requests() !== sent || again.summary.asserted !== 10)
    fail("a second pass over the same run reuses every answer", again);

  // One line of format.mjs changes: its statements are asked again, cart.mjs's
  // answers are reused.
  write("src/format.mjs", readFileSync(resolve(root, "src/format.mjs"), "utf8").replace("toUpperCase()", "toLocaleUpperCase()"));
  requireSupercov(root, ["--", "npm", "test"], { env: binary });
  const unassessed = JSON.parse(requireSupercov(root, ["runs", latestRun(root), "assertions", "--json"], { env }).stdout).data;
  if (unassessed.assessed !== false || unassessed.lastAssessed?.percentage !== 90.9)
    fail("a new run reads as not assessed and names the last assessment", unassessed);
  const before = requests();
  const changed = assess();
  const asked = requests() - before;
  if (changed.summary.statements !== 11 || changed.summary.requests === 0 || changed.summary.answersReused < 7 || asked > 2)
    fail(`after editing format.mjs only its questions are asked again (${asked} requests)`, changed);
  console.log(`[assertions] 10 of 11 asserted, the console.log line not; a repeat pass sent nothing; editing format.mjs sent ${asked} requests and reused ${changed.summary.answersReused} answers`);
} finally {
  server.kill();
  rmSync(root, { recursive: true, force: true });
}
