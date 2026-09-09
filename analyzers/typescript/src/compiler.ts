import { createRequire } from "node:module";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/** Canonical Rust paths may carry a Windows namespace prefix. Node/compiler paths do not. */
export function analysisPath(path: string): string {
  return fileURLToPath(pathToFileURL(resolve(path)));
}

/** Use the project's semantics rather than silently substituting the analyzer's compiler version. */
export function projectCompilerPath(projectRoot: string): string {
  try {
    return createRequire(pathToFileURL(resolve(projectRoot, "package.json"))).resolve(
      "typescript",
    );
  } catch (error) {
    throw new Error(
      `Assertion analysis requires the project's TypeScript compiler API, including for JavaScript projects. TypeScript 5.8.3 is the tested compiler; install a compatible compiler in ${projectRoot}, rerun the tests, then query again.`,
      { cause: error },
    );
  }
}

export function loadProjectCompiler(
  projectRoot: string,
): typeof import("typescript") {
  const require = createRequire(pathToFileURL(resolve(projectRoot, "package.json")));
  let compiler: typeof import("typescript");
  try {
    compiler = require(projectCompilerPath(projectRoot));
  } catch (error) {
    throw new Error(
      `Cannot load the TypeScript compiler API from ${projectRoot}; install TypeScript in that project or supply options.typescript.`,
      { cause: error },
    );
  }
  if (
    typeof compiler.createProgram !== "function" ||
    typeof compiler.parseJsonConfigFileContent !== "function"
  ) {
    throw new Error(
      `The TypeScript installation in ${projectRoot} does not expose the compiler API required by this analyzer (createProgram and parseJsonConfigFileContent). TypeScript 5.8.3 is tested; TypeScript 7's different API is not supported. No fallback compiler was substituted.`,
    );
  }
  return compiler;
}
