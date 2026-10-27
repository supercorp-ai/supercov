// Regenerate the vendored Kotlin parser from its grammar.
//
//   node scripts/kotlin-grammar.mjs          write src/parser.c from grammar.js
//   node scripts/kotlin-grammar.mjs --check  fail when they do not match
//
// crates/supercov-engine/vendor/tree-sitter-kotlin holds the grammar Supercov
// fixes and the parser generated from it; the scanner is hand-written C and
// needs no generating. The tree-sitter CLI is pinned to the version upstream
// generated 1.1.0 with, which reproduces its parser byte for byte.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const CLI = 'tree-sitter-cli@0.24.7';
const grammar = resolve(import.meta.dirname, '..', 'crates/supercov-engine/vendor/tree-sitter-kotlin');
const work = mkdtempSync(join(tmpdir(), 'supercov-kotlin-grammar-'));
try {
  for (const file of ['grammar.js', 'tree-sitter.json']) copyFileSync(join(grammar, file), join(work, file));
  const generated = spawnSync('npx', ['-y', CLI, 'generate'], { cwd: work, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  if (generated.status !== 0) {
    console.error(`[kotlin-grammar] ${CLI} generate failed:\n${generated.stdout}${generated.stderr}`);
    process.exit(1);
  }
  const fresh = join(work, 'src/parser.c');
  const vendored = join(grammar, 'src/parser.c');
  if (process.argv.includes('--check')) {
    if (!readFileSync(fresh).equals(readFileSync(vendored))) {
      console.error('[kotlin-grammar] src/parser.c is not what grammar.js generates; run node scripts/kotlin-grammar.mjs');
      process.exit(1);
    }
    console.log('[kotlin-grammar] src/parser.c is what grammar.js generates');
  } else {
    copyFileSync(fresh, vendored);
    console.log('[kotlin-grammar] wrote src/parser.c');
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}
