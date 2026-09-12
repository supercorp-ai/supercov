#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { analyze } from "../dist/analyze.js";

const help =
  "Usage: supercov-asserted-typescript --config <analysis.json> [--output <facts.json>]\nConfig: { projectRoot, inputDirectory, sourceDir?, testDir?, tsconfig?, coverageRunner?, runtimeObservations? }\nRelative projectRoot and inputDirectory paths resolve from the config file; other paths resolve from the project.\nWithout --output, JSON facts go to stdout. Diagnostics always go to stderr.";

try {
  const args = process.argv.slice(2);
  if (args.includes("--help")) {
    console.log(help);
  } else {
    let configPath;
    let outputPath;
    for (let i = 0; i < args.length; i += 2) {
      if (!args[i + 1] || args[i + 1].startsWith("--")) throw new Error(help);
      if (args[i] === "--config" && !configPath)
        configPath = resolve(args[i + 1]);
      else if (args[i] === "--output" && !outputPath)
        outputPath = resolve(args[i + 1]);
      else throw new Error(`Unknown or duplicate option ${args[i]}\n${help}`);
    }
    if (!configPath) throw new Error(help);
    const config = JSON.parse(readFileSync(configPath, "utf8"));
    if (
      typeof config.projectRoot !== "string" ||
      typeof config.inputDirectory !== "string"
    )
      throw new Error("projectRoot and inputDirectory are required strings");
    if ("typescript" in config)
      throw new Error(
        "typescript is a programmatic compiler-API option, not a JSON option",
      );
    const result = analyze({
      ...config,
      projectRoot: resolve(dirname(configPath), config.projectRoot),
      inputDirectory: resolve(dirname(configPath), config.inputDirectory),
    });
    const json = JSON.stringify(result.facts, null, 2) + "\n";
    if (outputPath) writeFileSync(outputPath, json, { flag: "wx" });
    else process.stdout.write(json);
    console.error(JSON.stringify(result.diagnostics));
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
