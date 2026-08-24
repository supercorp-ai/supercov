import { rename, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { build } from "esbuild";

const output = resolve("dist/runtime.js");
const temporary = resolve("dist/runtime.compat.js");
const result = await build({
  entryPoints: [output],
  bundle: true,
  format: "esm",
  platform: "neutral",
  target: "es2017",
  write: false,
  legalComments: "none",
});
const code = result.outputFiles[0]?.contents;
if (!code) throw new Error("esbuild did not produce the Supercov runtime");
await writeFile(temporary, code);
await rename(temporary, output);
