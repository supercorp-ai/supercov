import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { projectCompilerPath } from "../dist/compiler.js";

/** Hash the native implementation as well as its JS client; version.cjs alone is not the compiler. */
export function compilerIdentity(projectRoot) {
  return compilerIdentityFromEntry(projectCompilerPath(projectRoot));
}

export function compilerIdentityFromEntry(entry) {
  const require = createRequire(pathToFileURL(entry));
  const { version } = require(entry);
  const entrySha256 = createHash("sha256")
    .update(readFileSync(entry))
    .digest("hex");
  if (version !== "7.0.2")
    return { backend: "typescript-legacy", version, sha256: entrySha256 };
  const packageRoot = dirname(require.resolve("typescript/package.json"));
  const platform = `@typescript/typescript-${process.platform}-${process.arch}`;
  let nativeRoot;
  try {
    nativeRoot = dirname(require.resolve(`${platform}/package.json`));
  } catch (cause) {
    throw new Error(
      `TypeScript 7's native compiler dependency ${platform} is missing. Install the project's optional TypeScript dependencies before recapturing tests.`,
      { cause },
    );
  }
  const nativeMetadata = JSON.parse(
    readFileSync(resolve(nativeRoot, "package.json"), "utf8"),
  );
  if (nativeMetadata.name !== platform || nativeMetadata.version !== version)
    throw new Error(
      `TypeScript native package identity mismatch: expected ${platform}@${version}`,
    );
  const executable = resolve(
    nativeRoot,
    "lib",
    process.platform === "win32" ? "tsc.exe" : "tsc",
  );
  const nativeSha256 = createHash("sha256")
    .update(readFileSync(executable))
    .digest("hex");
  const hash = createHash("sha256");
  let files = 0;
  function walk(root, relative = "") {
    for (const file of readdirSync(resolve(root, relative), {
      withFileTypes: true,
    }).sort((a, b) => a.name.localeCompare(b.name))) {
      if (file.name === "node_modules") continue;
      const path = relative ? `${relative}/${file.name}` : file.name;
      if (file.isDirectory()) walk(root, path);
      else if (file.isFile()) {
        hash
          .update(path)
          .update("\0")
          .update(readFileSync(resolve(root, path)))
          .update("\0");
        files++;
      } else throw new Error(`Unsupported compiler package entry: ${path}`);
    }
  }
  hash.update("typescript-package\0");
  walk(packageRoot);
  hash.update("native-package\0");
  walk(nativeRoot);
  return {
    backend: "typescript-native-7",
    version,
    sha256: hash.digest("hex"),
    nativePackage: platform,
    nativeSha256,
    files,
  };
}
