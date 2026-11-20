#!/bin/bash
# Diagnosis (shell-diag scenario): hono's five extra kills, each mutant applied by hand and the
# killing test run with plain Vitest: alone, in its whole file, and the file with the baseline.
V="node_modules/.bin/vitest run --config vitest.bench.config.ts --reporter=default"
run() { echo "### $1"; shift; "$@" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Test Files|Tests  |✗|×|FAIL" | head -8; }
mutate() { node -e 'const fs=require("fs");const [f,l,a,b]=process.argv.slice(1);const L=fs.readFileSync(f,"utf8").split("\n");if(!L[l-1].includes(a))throw new Error("no match on line "+l+": "+L[l-1]);L[l-1]=L[l-1].replace(a,b);fs.writeFileSync(f,L.join("\n"))' "$@"; sed -n "$2p" "$1"; }
restore() { git checkout -q -- src; }

echo "======== baseline"
run "Should be a read-only alone" $V src/hono.test.ts -t "Should be a read-only"
run "ErrorBoundary > streaming > no error alone" $V src/jsx/components.test.tsx -t "no error"
run "components.test.tsx whole" $V src/jsx/components.test.tsx
run "render to stream alone" $V src/jsx/streaming.test.tsx -t "render to stream"

echo; echo "======== router.ts:14:9 !this.#routes -> false"
mutate src/router/smart-router/router.ts 14 '!this.#routes' 'false'
run "Should be a read-only alone" $V src/hono.test.ts -t "Should be a read-only"
run "hono.test.ts whole" $V src/hono.test.ts
restore

echo; echo "======== html.ts:83 [] -> [\"Stryker was here\"]"
mutate src/utils/html.ts 83 '|| [])' '|| ["Stryker was here"])'
run "render to stream alone" $V src/jsx/streaming.test.tsx -t "render to stream"
run "streaming.test.tsx whole" $V src/jsx/streaming.test.tsx
run "main suite" $V
restore

for spec in "100#match === -1#false" "100#match === -1#match === +1" "157#typeof str === 'object'#true"; do
  IFS='#' read -r line from to <<< "$spec"
  echo; echo "======== html.ts:$line $from -> $to"
  mutate src/utils/html.ts "$line" "$from" "$to"
  run "ErrorBoundary > streaming > no error alone" $V src/jsx/components.test.tsx -t "no error"
  run "components.test.tsx whole" $V src/jsx/components.test.tsx
  run "main suite" $V
  restore
done
