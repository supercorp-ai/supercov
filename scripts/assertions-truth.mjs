#!/usr/bin/env node
// Ground truth for an assertion assessment: for each statement the latest run
// assessed, make its change in every way the question allows, run the tests,
// and see whether any version fails them. Then score the assessment against
// that and print the row docs/assertions.md tabulates.
//
//   node scripts/assertions-truth.mjs <project> [--sample N] [--workers N]
//        [--out truth.json] -- <test command...>
//
// Go and Ruby. A statement no version of which builds (deleting a declaration
// Go then misses) is left out; a version that makes the tests hang counts as
// caught. The project is copied once per worker, and never changed.
import { spawn, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { latestRun } from "./coverage-test-helpers.mjs";

const args = process.argv.slice(2);
const split = args.indexOf("--");
if (split < 1) {
  console.error("usage: node scripts/assertions-truth.mjs <project> [--sample N] [--workers N] [--out file] -- <test command...>");
  process.exit(2);
}
const command = args.slice(split + 1);
const options = args.slice(1, split);
const option = (name, fallback) => {
  const at = options.indexOf(name);
  return at < 0 ? fallback : options[at + 1];
};
const project = resolve(args[0]);
const sample = Number(option("--sample", 0));
const workers = Number(option("--workers", 8));
const out = option("--out", undefined);

const run = latestRun(project);
const assessment = JSON.parse(
  readFileSync(join(project, ".supercov/runs", run, "assertion-coverage.json"), "utf8"),
);
const go = existsSync(join(project, "go.mod"));
const ruby = !go && assessment.statements.some((s) => s.file.endsWith(".rb"));
if (!go && !ruby) {
  console.error("Only Go and Ruby projects are supported.");
  process.exit(2);
}

// A pre-registered sample: the same statements every time for one run.
let statements = assessment.statements;
if (sample && sample < statements.length) {
  let seed = 7;
  const random = () => ((seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  statements = statements
    .map((statement) => [random(), statement])
    .sort((a, b) => a[0] - b[0])
    .map(([, statement]) => statement)
    .slice(0, sample);
}

// Go -------------------------------------------------------------------------

const ZERO = "func supercovTruthZero[T any](_ T) T { var zero T; return zero }\n";
const indent = (line) => line.slice(0, line.length - line.trimStart().length);

// Where the brackets the statement opens close: its text holds only its
// first line. Strings, runes and comments are skipped.
function extent(lines, first) {
  let depth = 0;
  for (let index = first; index < lines.length; index++) {
    const line = lines[index];
    let quote = null;
    for (let i = 0; i < line.length; i++) {
      const c = line[i];
      if (quote) {
        if (c === "\\" && quote !== "`") i++;
        else if (c === quote) quote = null;
      } else if (c === '"' || c === "'" || c === "`") quote = c;
      else if (line.startsWith("//", i)) break;
      else if ("([{".includes(c)) depth++;
      else if (")]}".includes(c)) depth--;
    }
    if (depth <= 0 && quote !== "`") return index;
  }
  return first;
}

// The names a statement's first line assigns.
function assigned(first) {
  const code = first.trim();
  if (code.startsWith("var ")) {
    return code.slice(4).split("=")[0].split(",").map((name) => name.trim().split(" ")[0]);
  }
  const match =
    code.match(/^(.+?)\s*(:=|\+=|-=|\*=|\/=|%=|&=|\|=|\^=|<<=|>>=|&\^=|=)(?!=)/) ??
    code.match(/^(.+?)(\+\+|--)$/);
  return match ? match[1].split(",").map((name) => name.trim()) : [];
}

function goVersions(statement, text) {
  const lines = text.split("\n");
  const first = statement.line - 1;
  const last = extent(lines, first);
  const blank = Array(last - first + 1).fill("");
  const around = (replacement) => [...lines.slice(0, first), ...replacement, ...lines.slice(last + 1)];
  switch (statement.change) {
    case "condition inverted": {
      const match = lines[first].match(/^(\s*(?:\}\s*else\s+)?if\s+)(.*)\{\s*$/);
      if (!match) return [];
      let [, head, rest] = match;
      let init = "";
      if (rest.includes(";")) {
        init = `${rest.slice(0, rest.lastIndexOf(";"))}; `;
        rest = rest.slice(rest.lastIndexOf(";") + 1);
      }
      rest = rest.trim();
      return [`(${rest}) && false`, `(${rest}) || true`].map((condition) =>
        [...lines.slice(0, first), `${head}${init}${condition} {`, ...lines.slice(first + 1)].join("\n"),
      );
    }
    case "skipped":
      return [around(blank).join("\n")];
    case "returns zero values":
      // The line Supercov wrote out, which still evaluates the original.
      return statement.replacement
        ? [around([indent(lines[first]) + statement.replacement, ...blank.slice(1)]).join("\n")]
        : [];
    case "value becomes the zero value": {
      const zeroed = assigned(lines[first])
        .filter((name) => name && name !== "_")
        .map((name) => `${name} = supercovTruthZero(${name})`)
        .join("; ");
      return zeroed
        ? [[...lines.slice(0, last + 1), indent(lines[first]) + zeroed, ...lines.slice(last + 1)].join("\n")]
        : [];
    }
    default:
      return [];
  }
}

function prepareGo(copy) {
  // The zero helper is generic, which needs a go directive of 1.18 or later;
  // none at all reads as 1.16.
  const directive = readFileSync(join(copy, "go.mod"), "utf8").match(/^go (\d+)\.(\d+)/m);
  if (!directive || Number(directive[1]) * 100 + Number(directive[2]) < 118) {
    spawnSync("go", ["mod", "edit", "-go=1.18"], { cwd: copy });
  }
}

// Ruby -----------------------------------------------------------------------

const mutate = join(dirname(new URL(import.meta.url).pathname), "assertions-truth.rb");
function rubyVersions(statement, path) {
  const made = spawnSync("ruby", [mutate, path, String(statement.line), statement.change], {
    encoding: "utf8",
  });
  return made.status === 0 ? JSON.parse(made.stdout) : [];
}

// Running --------------------------------------------------------------------

function execute(name, argv, cwd, timeout) {
  return new Promise((done) => {
    const child = spawn(name, argv, { cwd, stdio: ["ignore", "pipe", "pipe"], detached: true });
    let output = "";
    child.stdout.on("data", (chunk) => (output += chunk));
    child.stderr.on("data", (chunk) => (output += chunk));
    const timer = setTimeout(() => {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {}
      done({ hung: true, output });
    }, timeout);
    child.on("close", (status) => {
      clearTimeout(timer);
      done({ status, output });
    });
  });
}

async function judge(copy, statement) {
  const path = join(copy, statement.file);
  const original = readFileSync(path);
  const helper = join(dirname(path), "zz_supercov_truth_zero.go");
  const results = [];
  try {
    const versions = go ? goVersions(statement, original.toString("utf8")) : rubyVersions(statement, path);
    for (const text of versions) {
      writeFileSync(path, text);
      if (go) {
        const pkg = original.toString("utf8").match(/^package (\w+)/m)[1];
        writeFileSync(helper, `package ${pkg}\n\n${ZERO}`);
      } else if (spawnSync("ruby", ["-c", path]).status !== 0) {
        results.push("invalid");
        continue;
      }
      const ran = await execute(command[0], command.slice(1), copy, 120_000);
      if (ran.hung) results.push("caught");
      else if (go && (/\[build failed\]|setup failed/.test(ran.output) || (ran.status !== 0 && !ran.output.includes("FAIL")))) {
        results.push("invalid");
      } else results.push(ran.status === 0 ? "survived" : "caught");
    }
  } finally {
    writeFileSync(path, original);
    rmSync(helper, { force: true });
  }
  const valid = results.filter((result) => result !== "invalid");
  return {
    file: statement.file,
    line: statement.line,
    change: statement.change,
    versions: results,
    asserted: valid.length ? valid.includes("caught") : null,
  };
}

const scratch = mkdtempSync(join(tmpdir(), "supercov-truth-"));
const copies = [];
for (let i = 0; i < workers; i++) {
  const copy = join(scratch, String(i));
  cpSync(project, copy, {
    recursive: true,
    verbatimSymlinks: true,
    filter: (source) => ![".supercov", ".git"].includes(basename(source)),
  });
  if (go) prepareGo(copy);
  copies.push(copy);
}
const truths = new Array(statements.length);
let next = 0;
await Promise.all(
  copies.map(async (copy) => {
    while (next < statements.length) {
      const index = next++;
      truths[index] = await judge(copy, statements[index]);
    }
  }),
);
rmSync(scratch, { recursive: true, force: true });
if (out) writeFileSync(out, `${JSON.stringify(truths, null, 1)}\n`);

// Scoring --------------------------------------------------------------------

const verdicts = new Map(assessment.statements.map((s) => [`${s.file}:${s.line}`, s.asserted]));
const checked = truths.filter((truth) => truth.asserted !== null);
const right = checked.filter((truth) => verdicts.get(`${truth.file}:${truth.line}`) === truth.asserted);
const flagged = checked.filter((truth) => !verdicts.get(`${truth.file}:${truth.line}`));
const percent = (count) => `${((100 * count) / checked.length).toFixed(1)}%`;
const reported = checked.length - flagged.length;
const truly = checked.filter((truth) => truth.asserted).length;
const of = sample ? `${checked.length} of a sample of ${statements.length}` : `${checked.length} of ${statements.length}`;
console.log(
  `| ${basename(project)} | ${of} | ${right.length} | ${flagged.length} (${flagged.filter((t) => !t.asserted).length}) | ${percent(reported)} / ${percent(truly)} |`,
);
