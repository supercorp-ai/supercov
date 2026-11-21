#!/bin/bash
# Diagnosis (shell-diag scenario): three hono mutants that Supercov's af7340e5 binary overturned
# from killed to survived. Each applied by hand, run with plain Vitest: the killing tests alone,
# their files whole, and the whole suite.
V="node_modules/.bin/vitest run --config vitest.bench.config.ts --reporter=default"
run() { echo "### $1"; shift; "$@" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Test Files|Tests  |✗|×|FAIL" | head -8; }
mutate() { node -e 'const fs=require("fs");const [f,l,a,b]=process.argv.slice(1);const L=fs.readFileSync(f,"utf8").split("\n");if(!L[l-1].includes(a))throw new Error("no match on line "+l+": "+L[l-1]);L[l-1]=L[l-1].replace(a,b);fs.writeFileSync(f,L.join("\n"))' "$@"; sed -n "$2p" "$1"; }
restore() { git checkout -q -- src; }
check() {
  run "render to stream alone" $V src/jsx/streaming.test.tsx -t "render to stream"
  run "streaming.test.tsx whole" $V src/jsx/streaming.test.tsx
  run "components.test.tsx whole" $V src/jsx/components.test.tsx
  run "both files" $V src/jsx/streaming.test.tsx src/jsx/components.test.tsx
  run "main suite" $V
}
echo "======== baseline"; check
echo; echo "======== html.ts:82 typeof r === 'object' -> true"; mutate src/utils/html.ts 82 "typeof r === 'object'" "true"; check; restore
echo; echo "======== html.ts:100-103 block emptied"
node -e 'const fs=require("fs");const f="src/utils/html.ts";const s=fs.readFileSync(f,"utf8");const from="if (match === -1) {\n    buffer[0] += str\n    return\n  }";if(s.split(from).length!==2)throw new Error("no match");fs.writeFileSync(f,s.replace(from,"if (match === -1) {}"))'; sed -n 99,101p src/utils/html.ts; check; restore
echo; echo "======== html.ts:185 preserveCallbacks -> false"; mutate src/utils/html.ts 185 "if (preserveCallbacks)" "if (false)"; check; restore
