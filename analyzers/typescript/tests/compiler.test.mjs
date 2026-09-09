import test from "node:test";
import assert from "node:assert/strict";
import { resolve, toNamespacedPath } from "node:path";
import { realpathSync } from "node:fs";
import {
  analysisPath,
  loadProjectCompiler,
  projectCompilerPath,
} from "../dist/compiler.js";

test("project compiler lookup accepts native canonical Windows paths", () => {
  const root = realpathSync(resolve(import.meta.dirname, ".."));
  const namespaced = toNamespacedPath(root);
  assert.equal(projectCompilerPath(namespaced), projectCompilerPath(root));
  assert.equal(analysisPath(namespaced), analysisPath(root));
  assert.equal(loadProjectCompiler(namespaced).version, "5.8.3");
});
