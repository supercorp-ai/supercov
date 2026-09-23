// The release tooling refused to bump 1.1.0 because a third-party crate sat at
// the same version. It was right to: replacing every quoted 1.1.0 in Cargo.toml
// would have pinned tree-sitter-kotlin-ng at a version that does not exist.
import assert from "node:assert/strict";
import test from "node:test";

import { replaceCargoLock, replaceCargoToml } from "../../scripts/bump-version.mjs";

const MANIFEST = `[workspace.package]
version = "1.1.0"

[workspace.dependencies]
supercov-contracts = { version = "=1.1.0", path = "crates/supercov-contracts" }
supercov-engine = { version = "=1.1.0", path = "crates/supercov-engine" }
ra_ap_syntax = "=0.0.349"
tree-sitter-kotlin-ng = "1.1.0"
`;

test("a manifest moves Supercov's own versions and nobody else's", () => {
  const { text, count } = replaceCargoToml(MANIFEST, "1.1.0", "1.1.1");
  assert.equal(count, 3, "the workspace and the two crates it pins by path");
  assert.match(text, /^version = "1\.1\.1"$/m);
  assert.match(text, /supercov-contracts = \{ version = "=1\.1\.1"/);
  assert.match(text, /supercov-engine = \{ version = "=1\.1\.1"/);
  assert.match(
    text,
    /tree-sitter-kotlin-ng = "1\.1\.0"/,
    "a dependency that happens to share the release's version stays where it is",
  );
  assert.match(text, /ra_ap_syntax = "=0\.0\.349"/, "and a pin of another shape is untouched");
});

const LOCKFILE = `[[package]]
name = "supercov-engine"
version = "1.1.0"

[[package]]
name = "tree-sitter-kotlin-ng"
version = "1.1.0"
`;

test("a lockfile moves the crates Supercov publishes and nobody else's", () => {
  const { text, count } = replaceCargoLock(LOCKFILE, "1.1.0", "1.1.1");
  assert.equal(count, 1);
  assert.match(text, /name = "supercov-engine"\nversion = "1\.1\.1"/);
  assert.match(text, /name = "tree-sitter-kotlin-ng"\nversion = "1\.1\.0"/);
});
