/** Source inventory supplied by the frontend. Positions are original-source UTF-16 offsets. */
export interface Site {
  id: string;
  file: string;
  kind: "decision" | "effect";
  category: string;
  classification: "contractual" | "incidental" | "review";
  start: { line: number; column: number };
  end: { line: number; column: number };
  pos: number;
  endPos: number;
  text: string;
  fn: string;
  owner: string;
  exported: boolean;
  note?: string;
  chain?: string[];
  method?: string;
  arg0?: string;
}

export interface AnalyzeOptions {
  projectRoot: string;
  /** Directory containing inventory.json and cov/ from the evidence converter. Read only. */
  inputDirectory?: string;
  /** Internal archive adapter input; callers of analyzeArchive never create converted files. */
  evidenceFiles?: Record<string, string>;
  /** Exact files selected by the archive integration, including JS and nonstandard test directories. */
  sourceFiles?: string[];
  testFiles?: string[];
  sourceDir?: string;
  testDir?: string;
  tsconfig?: string;
  /** The legacy node:test input uses transpiled V8 line numbers; archive input uses source lines. */
  coverageRunner?: "node" | "vitest";
  runtimeObservations?: boolean;
  /** Defaults to the compiler API installed in the project being analyzed. */
  typescript?: typeof import("typescript");
}

export interface AnalysisDiagnostics {
  runtimeTests: number;
  linkedTests: number;
  staticTests: number;
  linkedByAssertionLines: number;
  linkedByTitle: number;
  linkWarnings: string[];
  unrecognizedOperands: { shape: string; count: number }[];
  compilerVersion: string;
}
