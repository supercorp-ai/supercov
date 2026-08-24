import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { atomicRenameSync, atomicWriteFileSync } from "../../src/atomic.ts";

const temporaryDirectories: string[] = [];

function temporaryDirectory(): string {
  const directory = mkdtempSync(resolve(tmpdir(), "supercov-atomic-"));
  temporaryDirectories.push(directory);
  return directory;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

describe("atomic writes", () => {
  it("atomically creates and replaces complete files without temp debris", () => {
    const directory = temporaryDirectory();
    const path = resolve(directory, "nested/report.json");

    atomicWriteFileSync(path, '{"generation":1}\n');
    expect(existsSync(path)).toBe(true);
    expect(readFileSync(path, "utf8")).toBe('{"generation":1}\n');

    atomicWriteFileSync(path, '{"generation":2,"complete":true}\n');
    expect(readFileSync(path, "utf8")).toBe(
      '{"generation":2,"complete":true}\n',
    );
    expect(readdirSync(resolve(directory, "nested"))).toEqual(["report.json"]);
  });

  it("publishes a prepared directory with one rename", () => {
    const directory = temporaryDirectory();
    const staging = resolve(directory, "staging");
    const published = resolve(directory, "runs", "run-1");
    mkdirSync(staging, { recursive: true });
    writeFileSync(resolve(staging, "run.json"), "complete");

    atomicRenameSync(staging, published);

    expect(existsSync(staging)).toBe(false);
    expect(readFileSync(resolve(published, "run.json"), "utf8")).toBe("complete");
  });
});
