#!/bin/bash
# Diagnosis (run with the shell-diag scenario, DIAG_SCRIPT="bash $GITHUB_WORKSPACE/benchmarks/mutation/diag/hono-order.sh"): does each disputed hono mutant's kill
# depend on which tests run? Applies one mutant by hand, runs the killing test alone, with the
# covering subset StrykerJS runs, and in its whole file; restores the source after each.
V="node_modules/.bin/vitest run --config vitest.bench.config.ts --reporter=default"
run() { echo "### $1"; shift; "$@" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Test Files|Tests  |✗|×|FAIL" | head -8; }
mut() { echo; echo "======== $1"; }
restore() { git checkout -q -- src; }

mut "baseline (no mutant)"
run "read-only alone" $V src/hono.test.ts -t "Should be a read-only"
run "hono.test.ts whole" $V src/hono.test.ts
run "render to stream alone" $V src/jsx/streaming.test.tsx -t "render to stream"
run "JwtTokenIssuedAt + Issuer tests (no Infinity test)" $V src/utils/jwt/jwt.test.ts -t "JwtTokenIssuedAt\$|JwtTokenIssuer"

mut "router.ts:14 block emptied"
perl -0pi -e 's/if \(!this\.#routes\) \{\n\s*throw new Error\(MESSAGE_MATCHER_IS_ALREADY_BUILT\)\n\s*\}/if (!this.#routes) {}/' src/router/smart-router/router.ts
sed -n 13,17p src/router/smart-router/router.ts
run "read-only alone" $V src/hono.test.ts -t "Should be a read-only"
run "hono.test.ts whole" $V src/hono.test.ts
restore

for spec in "76#typeof r === 'object'#true" "63#i < 0#i <= 0" "81#typeof r === 'object'#true" "161#(str as string | Promise<string>) instanceof Promise#true"; do
  IFS='#' read -r line from to <<< "$spec"
  mut "html.ts:$line -> $to"
  node -e 'const fs=require("fs");const [f,l,a,b]=process.argv.slice(1);const L=fs.readFileSync(f,"utf8").split("\n");if(!L[l-1].includes(a))throw new Error("no match on line "+l+": "+L[l-1]);L[l-1]=L[l-1].replace(a,b);fs.writeFileSync(f,L.join("\n"))' src/utils/html.ts "$line" "$from" "$to"
  sed -n "${line}p" src/utils/html.ts
  run "render to stream alone" $V src/jsx/streaming.test.tsx -t "render to stream"
  run "streaming.test.tsx whole" $V src/jsx/streaming.test.tsx
  run "main suite (all files)" $V
  restore
done

mut "jwt.ts:156 now < payload.iat -> <="
sed -i '156s/now < payload.iat/now <= payload.iat/' src/utils/jwt/jwt.ts
sed -n 156p src/utils/jwt/jwt.ts
run "JwtTokenIssuedAt + Issuer tests (StrykerJS's covering set, no Infinity test)" $V src/utils/jwt/jwt.test.ts -t "JwtTokenIssuedAt\$|JwtTokenIssuer"
run "jwt.test.ts whole" $V src/utils/jwt/jwt.test.ts
restore
