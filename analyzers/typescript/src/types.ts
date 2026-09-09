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
  /** Query-local evidence supplied in memory by the archive adapter, using original-source positions. */
  evidenceFiles: Record<string, string>;
  /** Exact files selected by the archive integration, including JS and nonstandard test directories. */
  sourceFiles?: string[];
  testFiles?: string[];
  sourceDir?: string;
  testDir?: string;
  tsconfig?: string;
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
