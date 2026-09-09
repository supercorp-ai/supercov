/** Exercise the actual npm artifacts, not checkout-relative binaries or analyzers. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { createRequire } from "node:module";
import { resolve, sep } from "node:path";
import { nativePackageFor } from "../bin/native.js";
import { checkedIdentity } from "../analyzers/typescript/bin/identity.mjs";

const repository = resolve(import.meta.dirname, "..");
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-asserted-package-"));
const environment = { ...process.env };
for (const key of [
  "SUPERCOV_RUST_BINARY",
  "SUPERCOV_PACKAGE_ROOT",
  "NODE_PATH",
  "NODE_TEST_CONTEXT",
])
  delete environment[key];
const option = (name) => {
  const index = process.argv.indexOf(name);
  return index < 0 ? undefined : process.argv[index + 1];
};
const selected = nativePackageFor();
const binary = resolve(
  option("--binary") ??
    resolve(repository, "target/debug", selected.executable),
);
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
const digest = (file) =>
  createHash("sha256").update(readFileSync(file)).digest("hex");
function run(command, args, cwd = repository) {
  return spawnSync(command, args, {
    cwd,
    env: environment,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 60_000,
  });
}
function ok(result) {
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result.stdout;
}
function npm(args, cwd = repository) {
  return process.platform === "win32"
    ? run(
        process.env.ComSpec ?? "cmd.exe",
        ["/d", "/s", "/c", "npm.cmd", ...args],
        cwd,
      )
    : run("npm", args, cwd);
}
function pack(directory) {
  const [packed] = JSON.parse(
    ok(
      npm(
        ["pack", "--ignore-scripts", "--json", "--pack-destination", temporary],
        directory,
      ),
    ),
  );
  return { file: resolve(temporary, packed.filename), manifest: packed };
}

try {
  checkedIdentity();
  const main = pack(repository);
  const paths = new Set(main.manifest.files.map((f) => f.path));
  for (const file of [
    "bin/query.mjs",
    "bin/identity.mjs",
    "src/analyze.ts",
    "src/pragmas.ts",
    "src/compiler.ts",
    "dist/analyze.js",
    "dist/pragmas.js",
    "dist/build-identity.json",
    "package.json",
    "tsconfig.json",
  ])
    assert.ok(
      paths.has(`analyzers/typescript/${file}`),
      `missing shipped analyzer input: ${file}`,
    );
  assert.ok(paths.has("docs/assertion-evidence.md"));
  assert.ok(
    ![...paths].some(
      (p) =>
        p.includes("node_modules/") ||
        p.includes("/tests/") ||
        p.startsWith("docs/asserted-") ||
        p.startsWith("docs/supercov-bugs-"),
    ),
    "research fixtures and dependencies must not leak into the artifact",
  );
  const targets = json(resolve(repository, "npm/native-targets.json"));
  const target = targets.targets.find(
    (t) => t.package === selected.packageName,
  );
  assert.ok(target);
  const nativeRoot = ok(
    run(process.execPath, [
      resolve(repository, "scripts/package-native.mjs"),
      "--target",
      target.rustTarget,
      "--binary",
      binary,
      "--out",
      resolve(temporary, "native"),
    ]),
  ).trim();
  const native = pack(nativeRoot);
  const compilers = [
    ...new Set(
      [
        resolve(repository, "analyzers/typescript/node_modules/typescript"),
        resolve(repository, "node_modules/typescript"),
      ]
        .filter(existsSync)
        .map((path) => realpathSync(path)),
    ),
  ];
  assert.ok(
    compilers.length,
    "a development TypeScript compiler is required to build the fixture tarball",
  );
  assert.ok(
    compilers.some(
      (root) => json(resolve(root, "package.json")).version === "5.8.3",
    ),
    "Install the pinned analyzer toolchain: npm --prefix analyzers/typescript ci --ignore-scripts",
  );
  const results = [];
  for (const [index, compilerRoot] of compilers.entries()) {
    const compilerVersion = json(resolve(compilerRoot, "package.json")).version;
    const compiler = pack(compilerRoot);
    const consumer = resolve(temporary, `consumer-${index}`);
    cpSync(
      resolve(repository, "analyzers/typescript/tests/fixtures/archive"),
      consumer,
      { recursive: true },
    );
    const packageFile = resolve(consumer, "package.json");
    const specification = json(packageFile);
    specification.dependencies = {
      supercov: `file:${main.file}`,
      [selected.packageName]: `file:${native.file}`,
    };
    writeFileSync(packageFile, JSON.stringify(specification));
    ok(
      npm(
        [
          "install",
          "--ignore-scripts",
          "--legacy-peer-deps",
          "--no-audit",
          "--no-fund",
        ],
        consumer,
      ),
    );
    const installed = resolve(consumer, "node_modules/supercov");
    const launcher = resolve(installed, "bin/supercov.js");
    const cli = (args) => run(process.execPath, [launcher, ...args], consumer);
    const read = (args) => {
      const text = ok(cli(args));
      assert.ok(
        Buffer.byteLength(text) <= 65_536,
        "all public JSON responses stay bounded",
      );
      const envelope = JSON.parse(text);
      assert.equal(envelope.ok, true);
      return envelope.data;
    };
    const suite = () => {
      const before = new Set(
        existsSync(resolve(consumer, ".supercov/runs"))
          ? readdirSync(resolve(consumer, ".supercov/runs"))
          : [],
      );
      ok(cli(["--", "npm", "test"]));
      const added = readdirSync(resolve(consumer, ".supercov/runs")).filter(
        (id) => !before.has(id),
      );
      assert.equal(added.length, 1);
      return added[0];
    };
    assert.equal(existsSync(resolve(installed, "Cargo.toml")), false);
    assert.equal(
      digest(
        resolve(
          consumer,
          "node_modules",
          selected.packageName,
          "bin",
          selected.executable,
        ),
      ),
      digest(binary),
    );
    const noCompiler = suite();
    assert.match(
      cli(["runs", noCompiler, "asserted", "--json"]).stdout,
      /requires the project's TypeScript compiler API/,
    );
    ok(
      npm(
        [
          "install",
          "--ignore-scripts",
          "--legacy-peer-deps",
          "--no-audit",
          "--no-fund",
          `typescript@file:${compiler.file}`,
        ],
        consumer,
      ),
    );
    assert.match(
      cli(["runs", noCompiler, "asserted", "--json"]).stdout,
      /stale run/,
    );
    const compilerEntry = createRequire(packageFile).resolve("typescript");
    assert.ok(
      realpathSync(compilerEntry).startsWith(realpathSync(consumer) + sep),
      "compiler is installed, not linked to the checkout",
    );
    const runId = suite();
    assert.ok(read(["runs", runId, "--json"]).coverage.lines.total > 0);
    if (compilerVersion === "7.0.2") {
      const rejected = cli(["runs", runId, "asserted", "--json"]);
      assert.notEqual(rejected.status, 0);
      assert.match(
        rejected.stdout,
        /TypeScript 7's different API is not supported/,
      );
      results.push({
        compilerVersion,
        runId,
        ordinaryCoverage: true,
        assertionAnalysis: "rejected-incompatible-compiler",
      });
      continue;
    }
    const result = read([
      "runs",
      runId,
      "asserted",
      "--limit",
      "1000",
      "--json",
    ]);
    assert.equal(result.reportSchema, 2);
    assert.equal(result.assertionScore, null);
    assert.equal(
      result.sites.find((r) => r.site?.owner === "exact").candidate.status,
      "evident",
    );
    const evidence = (pointer, ...args) =>
      read([
        "runs",
        runId,
        "asserted",
        "--evidence",
        pointer,
        "--analysis",
        result.analysisId,
        "--json",
        ...args,
      ]);
    assert.equal(
      evidence("/diagnostics/compilerVersion").text,
      compilerVersion,
    );
    assert.ok(evidence("/executionLinks", "--limit", "1").items.length === 1);
    assert.equal(evidence("/tests", "--limit", "1").pagination.nextOffset, 1);
    assert.equal(
      read(["runs", runId, "asserted", "--pragmas", "--json"]).summary
        .analyzerSupported,
      2,
    );
    assert.match(
      cli(["runs", runId, "asserted", "--analysis", "0".repeat(64), "--json"])
        .stdout,
      /analysis changed/,
    );
    for (const file of ["src/pragmas.ts", "dist/pragmas.js"]) {
      const path = resolve(installed, "analyzers/typescript", file);
      const original = readFileSync(path, "utf8");
      writeFileSync(path, original + "\n// changed installed analyzer\n");
      assert.match(
        cli(["runs", runId, "asserted", "--json"]).stdout,
        /analyzer build is stale or modified/,
      );
      writeFileSync(path, original);
    }
    const tests = resolve(consumer, "tests/core.test.mjs");
    writeFileSync(
      tests,
      "// updated authored test source\n" + readFileSync(tests, "utf8"),
    );
    assert.match(
      cli(["runs", runId, "asserted", "--json"]).stdout,
      /tests changed/,
    );
    const next = suite();
    const refreshed = read(["runs", next, "asserted", "--json"]);
    assert.notEqual(refreshed.analysisId, result.analysisId);
    assert.equal(
      read(["runs", next, "asserted", "--pragmas", "--json"]).summary
        .analyzerSupported,
      2,
    );
    results.push({
      compilerVersion,
      runId,
      refreshedRun: next,
      sites: result.summary.sites,
      installedAnalyzer: true,
      nativeOverride: false,
    });
  }
  console.log(
    JSON.stringify(
      {
        package: main.manifest.name,
        version: main.manifest.version,
        files: paths.size,
        results,
      },
      null,
      2,
    ),
  );
} finally {
  if (process.env.SUPERCOV_KEEP_ASSERTED_PACKAGE)
    console.log(`Retained package fixture: ${temporary}`);
  else rmSync(temporary, { recursive: true, force: true });
}
