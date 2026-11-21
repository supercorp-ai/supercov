#!/bin/bash
# Diagnosis (shell-diag scenario): tRPC withPing.ts:37 `if (result.done)` -> `if (false)`, applied by
# hand with an evaluation counter, run with plain Vitest (no mutation tool) on the two files whose
# tests cover it. Per-test durations come from Vitest's JSON reporter.
V="node_modules/.bin/vitest run --config vitest.bench.config.ts"
F="packages/server/src/unstable-core-do-not-import/stream/jsonl.test.ts packages/server/src/unstable-core-do-not-import/router.mergeRouters.test.ts"
SRC=packages/server/src/unstable-core-do-not-import/stream/utils/withPing.ts
OUT=${DIAG_OUT:-/tmp}
summary() { node -e '
const r=JSON.parse(require("fs").readFileSync(process.argv[1]));
for (const f of r.testResults) for (const t of f.assertionResults) if (/withPing|heartbeats/.test(t.fullName)) console.log(`  ${t.status.padEnd(7)} ${String(Math.round(t.duration ?? -1)).padStart(7)} ms  ${t.fullName}`);
console.log(`  totals: ${r.numPassedTests} passed, ${r.numFailedTests} failed, ${r.numPendingTests} skipped; success ${r.success}`);' "$1" 2>&1; }
run() {
  local name=$1; shift
  local start=$(date +%s.%N)
  timeout -s KILL 200 $V $F --reporter=json --outputFile=$OUT/$name.json "$@" > $OUT/$name.out 2>&1
  local rc=$?
  local end=$(date +%s.%N)
  echo "### $name: exit $rc, $(echo "$end - $start" | bc) s wall"
  [ -f $OUT/$name.json ] && summary $OUT/$name.json || tail -5 $OUT/$name.out
  grep -iE "hanging|close timed out|did not exit|Timeout|Error" $OUT/$name.out | sed 's/\x1b\[[0-9;]*m//g' | sort | uniq -c | head -5
}

sed -n 30,44p $SRC
run baseline
run baseline-forks-1 --pool=forks --maxWorkers=1

# Counts every evaluation of line 37's condition, per process and per test (the file name of the
# test that is running is not known here, so the count is per worker process), original and mutated.
count() {  # $1 = expression to place in the condition (the original test, or false)
  git checkout -q -- $SRC
  node -e '
const fs=require("fs");const [f,out,expr]=process.argv.slice(1);const s=fs.readFileSync(f,"utf8");
const from="if (result.done) {";if(s.split(from).length!==2)throw new Error("expected one match");
const g="globalThis";
const counter="("+g+".__wpHits = ("+g+".__wpHits ?? 0) + 1, "+g+".__wpHook ??= ("+g+".process.on(\"exit\", () => "+g+".process.getBuiltinModule(\"node:fs\").appendFileSync(\""+out+"/wp-hits.log\", \"exit pid \" + "+g+".process.pid + \" hits \" + "+g+".__wpHits + \"\\n\")), true), ("+g+".__wpHits <= 5 || "+g+".__wpHits % 25 === 0) && "+g+".process.getBuiltinModule(\"node:fs\").appendFileSync(\""+out+"/wp-hits.log\", \"progress pid \" + "+g+".process.pid + \" hits \" + "+g+".__wpHits + \" at \" + Date.now() + \"\\n\"), "+expr+")";
fs.writeFileSync(f,s.replace(from,"if ("+counter+") {"));' $SRC $OUT "$1"
}
for kind in original mutated; do
  if [ $kind = original ]; then count "result.done"; else count "false"; fi
  rm -f $OUT/wp-hits.log
  run count-$kind --pool=forks
  echo "evaluations of line 37 ($kind; last logged count per worker process, logged every 25):"; [ -f $OUT/wp-hits.log ] && awk '{last[$3]=$5} END {for (p in last) {print "  pid " p ": " last[p]; t+=last[p]} print "  total (to the nearest 25 per process): " t}' $OUT/wp-hits.log
done
git checkout -q -- $SRC
