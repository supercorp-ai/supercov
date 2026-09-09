/** Test new query assets against a proved, unchanged native binary. Never publishes. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, resolve } from "node:path";

const [
  runId = process.env.SUPERCOV_NATIVE_RUN,
  inputDirectory = ".ci-native-inputs",
  target = process.env.SUPERCOV_NATIVE_ARTIFACT,
] = process.argv.slice(2);
assert.match(runId ?? "", /^\d+$/);
assert.ok(process.env.GITHUB_REPOSITORY);
const repository = resolve(import.meta.dirname, "..");
execFileSync("git", ["diff", "--exit-code", "HEAD", "--"], {
  cwd: repository,
  stdio: "pipe",
});
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const run = JSON.parse(
  execFileSync(
    "gh",
    ["api", `repos/${process.env.GITHUB_REPOSITORY}/actions/runs/${runId}`],
    { encoding: "utf8" },
  ),
);
assert.equal(run.path, ".github/workflows/native-artifacts.yml");
assert.equal(run.head_repository.full_name, process.env.GITHUB_REPOSITORY);
assert.equal(
  run.conclusion,
  "success",
  "Only a complete successful native matrix can be reused",
);
assert.match(run.head_sha, /^[a-f0-9]{40}$/);
const changed = execFileSync(
  "git",
  ["diff", "--name-only", run.head_sha, "HEAD"],
  { cwd: repository, encoding: "utf8" },
)
  .trim()
  .split("\n")
  .filter(Boolean);
const queryOnlyFiles = new Set([
  ".github/workflows/ci.yml",
  "README.md",
  "CHANGELOG.md",
  "scripts/asserted-compiler-probe.mjs",
  "scripts/asserted-packed-integration.mjs",
  "scripts/asserted-supergateway-recapture.mjs",
  "scripts/asserted-reused-native.mjs",
  "scripts/package-preflight.mjs",
]);
for (const file of changed)
  assert.ok(
    file.startsWith("analyzers/typescript/") ||
      file.startsWith("docs/") ||
      queryOnlyFiles.has(file),
    `Native reuse is not proved for changed file ${file}; run the full native matrix`,
  );
const artifact = json(resolve(inputDirectory, `${target}.checksums.json`));
assert.equal(artifact.schemaVersion, 3);
assert.equal(
  artifact.version,
  json(resolve(repository, "package.json")).version,
);
assert.equal(artifact.platform, process.platform);
assert.equal(artifact.arch, process.arch);
assert.equal(basename(artifact.npmTarball.file), artifact.npmTarball.file);
const tarball = resolve(inputDirectory, artifact.npmTarball.file);
const digest = (file) =>
  createHash("sha256").update(readFileSync(file)).digest("hex");
assert.equal(digest(tarball), artifact.npmTarball.sha256);
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-reused-native-"));
try {
  const paths = execFileSync("tar", ["-tzf", tarball], { encoding: "utf8" })
    .trim()
    .split(/\r?\n/);
  for (const path of paths)
    assert.ok(
      path.startsWith("package/") && !path.split(/[\\/]/).includes(".."),
    );
  execFileSync("tar", ["-xzf", tarball, "-C", temporary]);
  assert.equal(basename(artifact.executable), artifact.executable);
  const binary = resolve(temporary, "package/bin", artifact.executable);
  assert.equal(digest(binary), artifact.binary.sha256);
  console.log(
    JSON.stringify({
      reusedRun: runId,
      nativeCommit: run.head_sha,
      target: artifact.rustTarget,
      binarySha256: artifact.binary.sha256,
    }),
  );
  execFileSync(
    process.execPath,
    [
      resolve(repository, "scripts/asserted-packed-integration.mjs"),
      "--binary",
      binary,
    ],
    { cwd: repository, stdio: "inherit" },
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
