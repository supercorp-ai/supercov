import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, expect, it } from "vitest";
import {
  readEvidenceArchive,
  writeEvidenceArchive,
} from "../../src/evidenceArchive";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

it("packs raw evidence deterministically into one lossless gzip artifact", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-evidence-"));
  temporaryDirectories.push(root);
  const evidence = resolve(root, "evidence");
  mkdirSync(resolve(evidence, "worker-2"), { recursive: true });
  mkdirSync(resolve(evidence, "worker-1"), { recursive: true });
  writeFileSync(resolve(evidence, "worker-2/result.json"), '{"hit":2}\n');
  writeFileSync(resolve(evidence, "worker-1/result.json"), '{"hit":1}\n');

  const first = resolve(root, "first.json.gz");
  const second = resolve(root, "second.json.gz");
  const metadata = writeEvidenceArchive(evidence, first);
  writeEvidenceArchive(evidence, second);

  expect(readFileSync(first)).toEqual(readFileSync(second));
  expect(metadata.files).toBe(2);
  expect(metadata.compressedBytes).toBeLessThan(metadata.uncompressedBytes);
  expect(readEvidenceArchive(first).files).toEqual([
    { path: "worker-1/result.json", contents: '{"hit":1}\n' },
    { path: "worker-2/result.json", contents: '{"hit":2}\n' },
  ]);
});
