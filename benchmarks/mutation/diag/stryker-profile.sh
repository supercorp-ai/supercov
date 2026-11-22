#!/bin/bash
# Diagnosis (shell-diag scenario): where StrykerJS's mutant phase spends its time on a project,
# measured from its processes. Every Node process of the run loads stryker-forklog.cjs
# (NODE_OPTIONS), which records when it starts and ends, what it forks, and in a Vitest worker the
# test file each "run" message names and when. StrykerJS runs as the cold scenario does, on the
# mutate set given as $STRYKER_MUTATE (comma-separated), with its log file.
#   DIAG_SCRIPT="bash <here>/stryker-profile.sh"   with DIAG_OUT and STRYKER_MUTATE set
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${DIAG_OUT:-/tmp}
mkdir -p "$OUT"
LOG=$OUT/processes.jsonl
: > "$LOG"
WS=$([ -f pnpm-workspace.yaml ] && echo -w)
pnpm add -D $WS @stryker-mutator/core@10.1.1 @stryker-mutator/vitest-runner@10.1.1 > "$OUT/stryker-install.log" 2>&1 || { tail -20 "$OUT/stryker-install.log"; exit 1; }
CPUS=$(node -p 'require("os").availableParallelism()')
node -e '
const [mutate, cpus, extra] = process.argv.slice(1);
const config = { packageManager: "pnpm", testRunner: "vitest", plugins: ["@stryker-mutator/vitest-runner"],
  coverageAnalysis: "perTest", concurrency: Number(cpus), incremental: false, checkers: [],
  mutate: mutate.split(","), reporters: ["json", "progress-append-only"], jsonReporter: { fileName: "reports/profile.json" },
  tempDirName: ".stryker-tmp", cleanTempDir: true, fileLogLevel: "info", ...JSON.parse(extra || "{}") };
require("fs").writeFileSync("stryker.profile.json", JSON.stringify(config, null, 2));' "$STRYKER_MUTATE" "$CPUS" "${STRYKER_EXTRA:-}"
cat stryker.profile.json
start=$(date +%s.%N)
SUPERCOV_FORKLOG=$LOG NODE_OPTIONS="--require $HERE/stryker-forklog.cjs" node_modules/.bin/stryker run stryker.profile.json > "$OUT/stryker.out" 2>&1
echo "stryker exit $? after $(echo "$(date +%s.%N) - $start" | bc) s"
grep -E "Initial test run|Done in" "$OUT/stryker.out" | sed 's/\x1b\[[0-9;]*m//g'
cp stryker.log "$OUT/" 2>/dev/null
cp reports/profile.json "$OUT/" 2>/dev/null
node "$HERE/stryker-profile-read.mjs" "$LOG" "$OUT/stryker.out" | tee "$OUT/profile.txt"
