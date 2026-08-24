import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { gzipSync } from "node:zlib";
import { afterEach, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  EVIDENCE_ARCHIVE_MAGIC,
  readEvidenceArchive,
  writeEvidenceArchive,
  writeEvidenceArchiveEntries,
} from "../../src/evidenceArchive.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function framedEntry(path: string, contents: Buffer, header?: string): Buffer {
  const encoded = Buffer.from(
    header ?? `${JSON.stringify({ path, bytes: contents.byteLength })}\n`,
  );
  const size = Buffer.alloc(4);
  size.writeUInt32BE(encoded.byteLength);
  return Buffer.concat([size, encoded, contents]);
}

function writeRawArchive(root: string, body: Buffer): string {
  const path = resolve(root, `raw-${Math.random()}.gz`);
  writeFileSync(path, gzipSync(body, { level: 9 }));
  return path;
}

it("packs raw evidence deterministically into one lossless gzip artifact", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-evidence-"));
  temporaryDirectories.push(root);
  const evidence = resolve(root, "evidence");
  mkdirSync(resolve(evidence, "worker-2"), { recursive: true });
  mkdirSync(resolve(evidence, "worker-1"), { recursive: true });
  writeFileSync(resolve(evidence, "worker-2/result.json"), '{"hit":2}\n');
  writeFileSync(resolve(evidence, "worker-1/result.json"), '{"hit":1}\n');
  const manifest = resolve(root, "manifest.json");
  writeFileSync(manifest, '{"decisions":[],"points":[],"branches":[]}\n');

  const first = resolve(root, "first.json.gz");
  const second = resolve(root, "second.json.gz");
  const sources = [
    { file: manifest, path: "manifest.json" },
    { directory: evidence },
  ];
  const metadata = writeEvidenceArchive(sources, first);
  writeEvidenceArchive(sources, second);

  expect(readFileSync(first)).toEqual(readFileSync(second));
  expect(metadata.files).toBe(3);
  expect(metadata.compressedBytes).toBeLessThan(metadata.uncompressedBytes);
  expect(readEvidenceArchive(first).files).toEqual([
    {
      path: "manifest.json",
      contents: '{"decisions":[],"points":[],"branches":[]}\n',
    },
    { path: "worker-1/result.json", contents: '{"hit":1}\n' },
    { path: "worker-2/result.json", contents: '{"hit":2}\n' },
  ]);
});

it("uses contract-defined code-point ordering instead of locale ordering", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-evidence-unicode-"));
  temporaryDirectories.push(root);
  const destination = resolve(root, "evidence.raw.gz");
  writeEvidenceArchiveEntries(
    [
      { path: "𐀀/result.json", contents: "astral" },
      { path: "manifest.json", contents: "{}" },
      { path: "é/result.json", contents: "accent" },
      { path: "\uE000/result.json", contents: "private-use" },
      { path: "a/result.json", contents: "ascii" },
    ],
    destination,
  );
  expect(readEvidenceArchive(destination).files.map((entry) => entry.path)).toEqual([
    "a/result.json",
    "manifest.json",
    "é/result.json",
    "\uE000/result.json",
    "𐀀/result.json",
  ]);
});

it("rejects unsafe, duplicate, and denominator-free archive writes", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-evidence-invalid-write-"));
  temporaryDirectories.push(root);
  const destination = resolve(root, "evidence.raw.gz");
  for (const path of ["", "/absolute", "../escape", "a/../escape", "a\\b"])
    expect(() =>
      writeEvidenceArchiveEntries([{ path, contents: "" }], destination)
    ).toThrow();
  expect(() =>
    writeEvidenceArchiveEntries(
      [
        { path: "manifest.json", contents: "{}" },
        { path: "manifest.json", contents: "{}" },
      ],
      destination,
    )
  ).toThrow();
  expect(() =>
    writeEvidenceArchiveEntries(
      [{ path: "worker/result.json", contents: "{}" }],
      destination,
    )
  ).toThrow();
});

it("rejects noncanonical, unsorted, duplicate, truncated, and manifest-free reads", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-evidence-invalid-read-"));
  temporaryDirectories.push(root);
  const magic = Buffer.from(EVIDENCE_ARCHIVE_MAGIC);
  const manifest = framedEntry("manifest.json", Buffer.from("{}"));
  const invalid = [
    Buffer.concat([
      magic,
      framedEntry(
        "manifest.json",
        Buffer.alloc(0),
        '{ "path":"manifest.json","bytes":0}\n',
      ),
    ]),
    Buffer.concat([magic, manifest, framedEntry("a/result.json", Buffer.alloc(0))]),
    Buffer.concat([magic, manifest, manifest]),
    Buffer.concat([magic, framedEntry("result.json", Buffer.from("{}"))]),
    Buffer.concat([magic, manifest.subarray(0, -1)]),
    Buffer.concat([magic, manifest, Buffer.from([0])]),
  ];
  for (const body of invalid)
    expect(() => readEvidenceArchive(writeRawArchive(root, body))).toThrow();
});
