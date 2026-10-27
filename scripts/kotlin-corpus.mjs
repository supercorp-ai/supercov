// How much real Kotlin the grammar Supercov instruments with reads.
//
//   node scripts/kotlin-corpus.mjs            check against the known failures
//   node scripts/kotlin-corpus.mjs --update   write the known failures anew
//
// Checks out pinned commits of real projects (into target/kotlin-corpus,
// kept between runs), parses every Kotlin file with the vendored grammar, and
// fails when a file fails that is not in scripts/kotlin-corpus-failures.txt.
// A listed file that now parses is said, so the list only shrinks. A file the
// grammar cannot read is a file Supercov leaves unmeasured.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const corpus = resolve(repository, 'target', 'kotlin-corpus');
const known = resolve(repository, 'scripts', 'kotlin-corpus-failures.txt');
const report = resolve(corpus, 'report.txt');
const PROJECTS = [
  ['Exposed', 'https://github.com/JetBrains/Exposed', '09f814c61c22851b76840ccb65c1ac2c4db97452'],
  ['kotlinx.coroutines', 'https://github.com/Kotlin/kotlinx.coroutines', '7e0d1b3e9ca83652427ae00dda7d1673f058a743'],
  ['ktor', 'https://github.com/ktorio/ktor', 'd96280165fc9890217196ed0dffd45228e35987d'],
  ['moshi', 'https://github.com/square/moshi', '889013ec2edb8d8034902662a1dc8c4f3b3f8111'],
  ['nowinandroid', 'https://github.com/android/nowinandroid', '2982e63cb691ad49cd009aab43dc206fee6699c2'],
  ['okhttp', 'https://github.com/square/okhttp', 'c3179301fc7d0266c5bf154870c2b0b9da01e7e7'],
];

const run = (cwd, command, args, env = {}) => {
  const result = spawnSync(command, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', env: { ...process.env, ...env } });
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed:\n${result.stdout}${result.stderr}`);
  return result.stdout.trim();
};

mkdirSync(corpus, { recursive: true });
for (const [name, url, commit] of PROJECTS) {
  const directory = resolve(corpus, name);
  if (existsSync(resolve(directory, '.git'))) {
    if (run(directory, 'git', ['rev-parse', 'HEAD']) === commit) continue;
  } else {
    mkdirSync(directory, { recursive: true });
    run(directory, 'git', ['init', '-q']);
  }
  run(directory, 'git', ['fetch', '-q', '--depth', '1', url, commit]);
  run(directory, 'git', ['checkout', '-q', '--force', 'FETCH_HEAD']);
}

run(repository, 'cargo', ['test', '-q', '--release', '-p', 'supercov-engine', '--test', 'kotlin_corpus', '--', '--ignored'], {
  SUPERCOV_KOTLIN_CORPUS: corpus,
  SUPERCOV_KOTLIN_REPORT: report,
});
const [total, ...lines] = readFileSync(report, 'utf8').split('\n').filter(Boolean);
const failing = new Map(lines.map((line) => line.split('\t')));
const percent = ((failing.size * 100) / Number(total)).toFixed(1);
console.log(`[kotlin-corpus] ${failing.size} of ${total} files do not parse (${percent}%)`);

if (process.argv.includes('--update')) {
  writeFileSync(known, [...failing.keys()].sort().map((file) => `${file}\n`).join(''));
  console.log(`[kotlin-corpus] wrote ${failing.size} known failures`);
  process.exit(0);
}
const listed = new Set(readFileSync(known, 'utf8').split('\n').filter(Boolean));
const fresh = [...failing.keys()].filter((file) => !listed.has(file));
const fixed = [...listed].filter((file) => !failing.has(file));
if (fixed.length > 0) {
  console.log(`[kotlin-corpus] ${fixed.length} listed file(s) parse now; take them off the list with --update:`);
  for (const file of fixed) console.log(`  ${file}`);
}
if (fresh.length > 0) {
  console.error(`[kotlin-corpus] ${fresh.length} file(s) no longer parse:`);
  for (const file of fresh) console.error(`  ${file}\t${failing.get(file)}`);
  process.exit(1);
}
