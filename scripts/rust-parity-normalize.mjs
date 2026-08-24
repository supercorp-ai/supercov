import { createHash } from "node:crypto";

const omittedDynamicKeys = new Set([
  "generatedAt",
  "integrity",
  "startedAtMs",
  "endedAtMs",
  "timestampMs",
]);
const selectedEngineImplementationFiles = new Set([
  "src/engineEvidence.ts",
  "src/engineInstrumenter.ts",
  "src/engineProcess.ts",
  "src/evidenceArchive.ts",
  "src/instrumenter.ts",
]);

export function collectAttemptIdentities(value, identities) {
  if (Array.isArray(value)) {
    for (const entry of value) collectAttemptIdentities(entry, identities);
    return;
  }
  if (!value || typeof value !== "object") return;
  const scope = value.scope;
  if (
    scope &&
    typeof scope === "object" &&
    typeof scope.attemptId === "string" &&
    typeof scope.testId === "string"
  ) {
    identities.set(
      scope.attemptId,
      `<attempt:${scope.testId}:retry-${scope.retry ?? 0}>`,
    );
  }
  for (const entry of Object.values(value))
    collectAttemptIdentities(entry, identities);
}

function canonicalString(value, context, key) {
  if (key === "workerId")
    value = value
      .replace(/^pid-\d+-worker-(\d+)$/, "pid-<runtime>-worker-$1")
      .replace(/^node:test-\d+$/, "node:test-<runtime>");
  let result = value
    .replaceAll(
      `${context.project}/supercov/workspace/${context.projectName}`,
      "<project>/supercov/workspace/<workspace>",
    )
    .replaceAll(context.run, "<run-id>")
    .replaceAll(context.project, "<project>");
  if (context.parityRoot)
    result = result.replaceAll(context.parityRoot, "<parity-root>");
  for (const [attempt, identity] of context.attempts)
    result = result.replaceAll(attempt, identity);
  return result;
}

function omitted(context, key) {
  return Boolean(
    (key && omittedDynamicKeys.has(key)) ||
      (context.omitTimestampCorrelation && key === "phases") ||
      (context.omitTimestampCorrelation && key === "totalPhases") ||
      (context.omitTimestampCorrelation &&
        (key === "browserFallback" || key === "serverFallback")),
  );
}

function unorderedArray(context, key) {
  return Boolean(
    key === "phases" ||
      key === "explicitPhases" ||
      (context.unorderedEvidence &&
        (key === "server" || key === "browser" || key === "runtime")),
  );
}

export function canonicalize(value, context, key) {
  if (omitted(context, key)) return undefined;
  if (Array.isArray(value)) {
    const entries = value
      .map((entry) => canonicalize(entry, context))
      .filter((entry) => entry !== undefined);
    return unorderedArray(context, key)
      ? entries.sort((left, right) =>
          JSON.stringify(left).localeCompare(JSON.stringify(right)),
        )
      : entries;
  }
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .map(([entryKey, entry]) => [
          entryKey,
          canonicalize(entry, context, entryKey),
        ])
        .filter(([, entry]) => entry !== undefined),
    );
  if (typeof value !== "string") return value;
  return canonicalString(value, context, key);
}

function updateCanonicalHash(hash, value, context, key) {
  if (omitted(context, key)) return false;
  if (Array.isArray(value)) {
    hash.update("A[");
    if (unorderedArray(context, key)) {
      const entries = value.map((entry) => canonicalDigest(entry, context)).sort();
      for (const entry of entries) hash.update(`E${entry.length}:${entry}`);
    } else {
      for (const entry of value) updateCanonicalHash(hash, entry, context);
    }
    hash.update("]");
    return true;
  }
  if (value && typeof value === "object") {
    hash.update("O{");
    for (const [entryKey, entry] of Object.entries(value)) {
      if (omitted(context, entryKey)) continue;
      const encodedKey = JSON.stringify(entryKey);
      hash.update(`K${encodedKey.length}:${encodedKey}`);
      updateCanonicalHash(hash, entry, context, entryKey);
    }
    hash.update("}");
    return true;
  }
  const normalized = typeof value === "string"
    ? canonicalString(value, context, key)
    : value;
  const encoded = JSON.stringify(normalized);
  hash.update(`V${encoded?.length ?? 0}:${encoded ?? "undefined"}`);
  return true;
}

export function canonicalDigest(value, context) {
  const hash = createHash("sha256");
  updateCanonicalHash(hash, value, context);
  return hash.digest("hex");
}

function records(archive, kind) {
  if (kind === "results")
    return archive.files
      .filter((entry) => /(?:^|\/)mcdc\.json$/.test(entry.path))
      .map((entry) => JSON.parse(entry.contents));
  const background = kind === "backgroundServer";
  return archive.files
    .filter((entry) =>
      background
        ? /^server\/background\/.*\.jsonl$/.test(entry.path)
        : entry.path.startsWith("server/") &&
          !entry.path.startsWith("server/background/") &&
          entry.path.endsWith(".jsonl"),
    )
    .flatMap((entry) =>
      entry.contents
        .split("\n")
        .filter(Boolean)
        .map((line) => JSON.parse(line)),
    );
}

function obligationFiles(archive) {
  const manifestEntry = archive.files.find((entry) => entry.path === "manifest.json");
  const manifest = manifestEntry ? JSON.parse(manifestEntry.contents) : {};
  const files = new Map();
  for (const decision of manifest.decisions ?? []) files.set(decision.id, decision.file);
  for (const point of manifest.points ?? []) files.set(point.id, point.file);
  for (const branch of manifest.branches ?? [])
    for (const alternative of branch.alternatives ?? [])
      files.set(alternative.id, branch.file);
  return files;
}

function evidenceFile(value, files) {
  return value?.meta?.file ?? files.get(value?.id ?? value?.meta?.id);
}

function implementationFile(value, files) {
  const file = evidenceFile(value, files);
  return selectedEngineImplementationFiles.has(file) ? file : undefined;
}

function stripRuntimeBatch(batch, files) {
  const decisions = (batch.decisions ?? []).filter(
    (snapshot) => !implementationFile(snapshot, files),
  );
  const hits = (batch.hits ?? []).filter(
    (id) => !selectedEngineImplementationFiles.has(files.get(id)),
  );
  const events = (batch.events ?? []).filter(
    (event) => !implementationFile(event, files),
  );
  if (decisions.length === 0 && hits.length === 0 && events.length === 0)
    return undefined;
  return {
    ...batch,
    decisions,
    hits,
    ...(batch.events === undefined ? {} : { events }),
  };
}

function withoutSelectedEngineEvidence(value, files) {
  return {
    ...value,
    server: (value.server ?? []).filter(
      (event) => !implementationFile(event, files),
    ),
    browser: (value.browser ?? [])
      .map((batch) => stripRuntimeBatch(batch, files))
      .filter(Boolean),
    runtime: (value.runtime ?? [])
      .map((batch) => stripRuntimeBatch(batch, files))
      .filter(Boolean),
  };
}

export function canonicalEvidenceSignatures(archive, context) {
  context.unorderedEvidence = true;
  const files = obligationFiles(archive);
  const results = records(archive, "results");
  const scopedServer = records(archive, "scopedServer");
  for (const value of [...results, ...scopedServer])
    collectAttemptIdentities(value, context.attempts);
  return {
    results: results
      .map((value) => ({
        key: `${value.testId}:retry-${value.retry ?? 0}`,
        testId: value.testId,
        signature: canonicalDigest(value, context),
        semanticSignature: canonicalDigest(
          withoutSelectedEngineEvidence(value, files),
          context,
        ),
        implementationFiles: [
          ...new Set(
            (value.server ?? [])
              .map((event) => implementationFile(event, files))
              .filter(Boolean),
          ),
        ].sort(),
      }))
      .sort((left, right) => left.key.localeCompare(right.key)),
    scopedServer: scopedServer
      .map((value) => ({
        testId: value.scope?.testId ?? "unscoped",
        signature: canonicalDigest(value, context),
      }))
      .sort((left, right) =>
        left.testId.localeCompare(right.testId) ||
        left.signature.localeCompare(right.signature),
      ),
    semanticScopedServer: scopedServer
      .filter((value) => !implementationFile(value, files))
      .map((value) => ({
        testId: value.scope?.testId ?? "unscoped",
        signature: canonicalDigest(value, context),
      }))
      .sort((left, right) =>
        left.testId.localeCompare(right.testId) ||
        left.signature.localeCompare(right.signature),
      ),
    backgroundServer: records(archive, "backgroundServer")
      .map((value) => canonicalDigest(value, context))
      .sort(),
  };
}
