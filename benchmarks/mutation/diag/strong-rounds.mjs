// Reads a supercov-diag artifact (plans, kept strong rounds, frontend-evidence/mutants.json) and reports, per disputed hono
// mutant: candidates, the copies that ran it, what the killing test reported, and the round-0 baseline.
//   node benchmarks/mutation/diag/strong-rounds.mjs <dir with a/ (artifact), x/ (diag.tgz) and r1..r3/ (rounds-N.tgz)>
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { join } from 'node:path';

const O = process.argv[2];
const targets = [
  { plan: 1, rounds: 1, id: 'src/router/smart-router/router.ts:14:24-16:6:BlockStatement:{}', killer: /Should be a read-only/, killerFile: 'src/hono.test.ts' },
  { plan: 1, rounds: 1, id: 'src/router/smart-router/router.ts:15:7-15:56:CallExpression:empty', killer: /Should be a read-only/, killerFile: 'src/hono.test.ts' },
  { plan: 2, rounds: 2, id: 'src/utils/html.ts:63:9-63:14:EqualityOperator:<=', killer: /render to stream/, killerFile: 'src/jsx/streaming.test.tsx' },
  { plan: 2, rounds: 2, id: 'src/utils/html.ts:76:9-76:30:ConditionalExpression:true', killer: /render to stream/, killerFile: 'src/jsx/streaming.test.tsx' },
  { plan: 2, rounds: 2, id: 'src/utils/html.ts:81:16-81:37:ConditionalExpression:true', killer: /render to stream/, killerFile: 'src/jsx/streaming.test.tsx' },
  { plan: 2, rounds: 2, id: 'src/utils/html.ts:161:9-161:61:ConditionalExpression:true', killer: /render to stream/, killerFile: 'src/jsx/streaming.test.tsx' },
  { plan: 3, rounds: 3, id: 'src/utils/jwt/jwt.ts:156:77-156:94:EqualityOperator:<=', killer: /JwtTokenIssuer \(none\)/, killerFile: 'src/utils/jwt/jwt.test.ts' },
];
const evidence = JSON.parse(readFileSync(join(O, 'x/.supercov/workspaces/workspace/hono/.supercov/frontend-evidence/mutants.json'), 'utf8'));
const plans = {};
const rounds = {};
for (const i of [1, 2, 3]) {
  plans[i] = JSON.parse(readFileSync(join(O, `a/plan-${i}.json`), 'utf8'));
  const list = [];
  const base = join(O, `r${i}`);
  for (const r of readdirSync(base)) {
    for (const h of readdirSync(join(base, r))) {
      for (const f of readdirSync(join(base, r, h)).filter((f) => f.endsWith('.json'))) {
        const copy = f.replace('.json', '');
        const assignment = JSON.parse(readFileSync(join(base, r, h, f), 'utf8'));
        const outFile = join(base, r, h, `${copy}.out`);
        const out = existsSync(outFile) ? readFileSync(outFile, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)) : null;
        list.push({ round: Number(r), hash: h, copy, assignment, out });
      }
    }
  }
  rounds[i] = list;
}

for (const t of targets) {
  const plan = plans[t.plan];
  const m = plan.mutants.find((x) => x.id === t.id);
  console.log(`\n=== ${t.id}`);
  if (!m) { console.log('not in plan; decided:', JSON.stringify(plan.decided?.find?.((x) => x.id === t.id) ?? null)); }
  const killerIdx = plan.tests.findIndex((x) => t.killer.test(x.name) && x.file === t.killerFile);
  const killer = plan.tests[killerIdx];
  console.log('killer test:', killerIdx, killer?.id, killer?.name, 'runnable', killer?.runnable, 'durationMs', killer?.durationMs);
  if (!m) continue;
  const fileList = evidence.files[m.file];
  const ev = fileList?.[m.index];
  console.log('plan: index', m.index, 'weak', JSON.stringify(m.weak), 'covering', m.covering, 'statement', m.statement, 'wholeFiles', JSON.stringify(m.wholeFiles));
  console.log('evidence mutants.json at index', m.index, JSON.stringify(ev));
  const candIdx = m.candidates.map((c) => c.test);
  const isCand = candIdx.includes(killerIdx);
  const candFiles = new Set(candIdx.map((i) => plan.tests[i].file));
  console.log('candidates', candIdx.length, 'files', candFiles.size, 'killer is candidate', isCand, isCand ? JSON.stringify(m.candidates.find((c) => c.test === killerIdx)) : '');
  console.log('candidate files include killer file', candFiles.has(t.killerFile));
  // copies with this mutant
  const copies = rounds[t.rounds].filter((c) => c.assignment.mutant && c.assignment.mutant.file === m.file && c.assignment.mutant.index === m.index);
  console.log('copies with this mutant:', copies.length);
  const allFailed = copies.reduce((a, c) => a + (c.out ?? []).filter((o) => o.test && o.failed).length, 0);
  console.log('  failed tests across all copies with this mutant:', allFailed);
  for (const c of copies) {
    const tests = c.assignment.tests ?? [];
    const fileOf0 = tests.map((id) => plan.tests.find((x) => x.id === id)?.file).filter(Boolean)[0];
    if (fileOf0 !== t.killerFile) continue;
    const hasKiller = killer && tests.includes(killer.id);
    const fileOf = tests.map((id) => plan.tests.find((x) => x.id === id)?.file).filter(Boolean);
    const testFile = fileOf[0] ?? '?';
    const outTests = (c.out ?? []).filter((o) => o.test);
    const failed = outTests.filter((o) => o.failed);
    const killerOut = killer ? outTests.find((o) => o.test === killer.id) : null;
    const flags = (c.out ?? []).filter((o) => !o.test);
    console.log(`  round ${c.round} copy ${c.hash}/${c.copy}: file ${testFile}, ${tests.length} tests assigned, wholeFile ${c.assignment.wholeFile}, killer assigned ${hasKiller}; reported ${outTests.length} tests, ${failed.length} failed; killer: ${JSON.stringify(killerOut)}; other lines ${JSON.stringify(flags)}`);
    if (failed.length) console.log('    failed:', failed.slice(0, 3).map((f) => plan.tests.find((x) => x.id === f.test)?.name ?? f.test).join(' | '));
  }
  // baseline round 0 copies of the killer's file
  const base = rounds[t.rounds].filter((c) => c.round === 0 && (c.assignment.tests ?? []).some((id) => plan.tests.find((x) => x.id === id)?.file === t.killerFile));
  for (const c of base) {
    const outTests = (c.out ?? []).filter((o) => o.test);
    const killerOut = killer ? outTests.find((o) => o.test === killer.id) : null;
    console.log(`  baseline round 0 ${c.hash}/${c.copy}: ${c.assignment.tests.length} tests, wholeFile ${c.assignment.wholeFile}, reported ${outTests.length}, failed ${outTests.filter((o) => o.failed).length}; killer ${JSON.stringify(killerOut)}; killer assigned ${killer ? c.assignment.tests.includes(killer.id) : '-'}`);
  }
  if (!base.length) console.log('  baseline round 0: no copy of', t.killerFile);
}
