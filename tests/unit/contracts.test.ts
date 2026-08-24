import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  AGENT_JSON_MAX_BYTES,
  AGENT_JSON_SCHEMA_VERSION,
  SUPERCOV_ERROR_CODES,
} from "../../src/agentJson.ts";
import {
  EVIDENCE_ARCHIVE_MAGIC,
  EVIDENCE_ARCHIVE_SCHEMA_VERSION,
} from "../../src/evidenceArchive.ts";
import {
  COMMAND_TERMINATION_GRACE_MS,
  COMMAND_TIMEOUT_EXIT_CODE,
  DEFAULT_DIAGNOSTIC_INTERVAL_MS,
  PROCESS_SUPERVISION_SCHEMA_VERSION,
} from "../../src/processDiagnostics.ts";
import { RUN_STORE_CONTRACT_VERSION } from "../../src/workspace.ts";
import { WAIVERS_FILE, WAIVERS_SCHEMA_VERSION } from "../../src/waivers.ts";

interface ContractRegistry {
  contractVersion: number;
  status: string;
  residentProcess: boolean;
  evidenceArchive: {
    schemaVersion: number;
    file: string;
    format: string;
    magic: string;
  };
  runStore: {
    schemaVersion: number;
    store: string;
    workspaceStore: string;
    publishedRunFiles: string[];
  };
  agentJson: {
    schemaVersion: number;
    maxBytes: number;
    defaultPageSize: number;
    errorCodes: string[];
  };
  waivers: { schemaVersion: number; file: string };
  processSupervision: {
    schemaVersion: number;
    diagnosticIntervalMs: number;
    timeoutExitCode: number;
    terminationGraceMs: number;
  };
}

const registry = JSON.parse(
  readFileSync(resolve("contracts/v1/contract.json"), "utf8"),
) as ContractRegistry;

describe("frozen engine contract v1", () => {
  it("pins every reference-engine schema and constant", () => {
    expect(registry).toMatchObject({
      contractVersion: 1,
      status: "frozen",
      residentProcess: false,
      evidenceArchive: {
        schemaVersion: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
        file: "evidence.raw.gz",
        format: "framed+gzip",
        magic: EVIDENCE_ARCHIVE_MAGIC,
      },
      runStore: {
        schemaVersion: RUN_STORE_CONTRACT_VERSION,
        store: ".supercov",
        workspaceStore: "supercov",
        publishedRunFiles: ["run.json", "evidence.raw.gz"],
      },
      agentJson: {
        schemaVersion: AGENT_JSON_SCHEMA_VERSION,
        maxBytes: AGENT_JSON_MAX_BYTES,
        defaultPageSize: 20,
        errorCodes: [...SUPERCOV_ERROR_CODES],
      },
      waivers: {
        schemaVersion: WAIVERS_SCHEMA_VERSION,
        file: WAIVERS_FILE,
      },
      processSupervision: {
        schemaVersion: PROCESS_SUPERVISION_SCHEMA_VERSION,
        diagnosticIntervalMs: DEFAULT_DIAGNOSTIC_INTERVAL_MS,
        timeoutExitCode: COMMAND_TIMEOUT_EXIT_CODE,
        terminationGraceMs: COMMAND_TERMINATION_GRACE_MS,
      },
    });
  });

  it("has no resident-process or serve contract", () => {
    const readme = readFileSync(resolve("contracts/v1/README.md"), "utf8");
    expect(registry.residentProcess).toBe(false);
    expect(readme).toContain("There is deliberately no server or daemon contract");
    expect(Object.keys(registry)).not.toContain("serve");
  });
});
