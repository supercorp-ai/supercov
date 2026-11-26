#!/usr/bin/env bash
# DIAGNOSTIC ONLY (never merged). One comparison on the generated large app,
# away from the Playwright collector: two of plain, base and new (Supercov
# binaries in $BINARIES/supercov-<name>/supercov), each built and served,
# then the flow driven against both (drive.mjs). Prints build CPU, client
# bytes, server CPU and the driver's results.
#   run.sh <A> <B> <mode: abba|trace> <count>
set -euo pipefail
A=$1; B=$2; MODE=$3; COUNT=$4
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${RUNNER_TEMP:-/tmp}/probe-diag
mkdir -p "$WORK"
SRC=$WORK/source
if [ ! -d "$SRC" ]; then
  node "$HERE/../large-app/generate.mjs" "$SRC" --tests 24 >/dev/null
  (cd "$SRC" && git init -q && npm install --no-audit --no-fund >/dev/null 2>&1 && npx playwright install chromium >/dev/null 2>&1 \
    && git add -A && git -c user.name=diag -c user.email=diag@example.com commit -qm generated)
fi
port_of() { case $1 in plain) echo 3401;; base) echo 3402;; new) echo 3403;; esac; }
start() {
  local v=$1 port; port=$(port_of "$v")
  local dir=$WORK/app-$v
  rm -rf "$dir"; cp -R "$SRC" "$dir"
  local timed="/usr/bin/time -f %e,%U,%S -o $WORK/build-$v.txt npx next build >$WORK/build-$v.log 2>&1"
  if [ "$v" = plain ]; then
    (cd "$dir" && sh -c "$timed && exec npx next start -p $port" >"$WORK/server-$v.log" 2>&1 &)
  else
    (cd "$dir" && "$BINARIES/supercov-$v/supercov" -- sh -c "$timed && exec npx next start -p $port" >"$WORK/server-$v.log" 2>&1 &)
  fi
}
start "$A"; start "$B"
for v in "$A" "$B"; do
  port=$(port_of "$v")
  for _ in $(seq 1 400); do curl -fsS -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null && break; sleep 1; done
  curl -fsS -o /dev/null "http://127.0.0.1:$port/" || { echo "server $v did not start"; tail -30 "$WORK/server-$v.log"; exit 1; }
  read -r wall user sys < <(tr ',' ' ' < "$WORK/build-$v.txt")
  chunks=$(find "$WORK/app-$v" -path '*/.next/static/chunks/*.js' -not -path '*/node_modules/*' -print0 | xargs -0 cat | wc -c)
  echo "build $v: wall ${wall}s, CPU $(echo "$user + $sys" | bc)s; client chunk bytes $chunks"
done
server_ticks() { # utime+stime of the next-server processes running in app-$1
  local total=0
  for pid in $(pgrep -f "next-server" || true); do
    if [ -r "/proc/$pid/stat" ] && readlink "/proc/$pid/cwd" | grep -q "/app-$1"; then
      read -r -a f < "/proc/$pid/stat"; total=$((total + f[13] + f[14]))
    fi
  done
  echo $total
}
beforeA=$(server_ticks "$A"); beforeB=$(server_ticks "$B")
URL_A="http://127.0.0.1:$(port_of "$A")"; URL_B="http://127.0.0.1:$(port_of "$B")"
node "$HERE/drive.mjs" "$MODE" "$WORK/app-$A" "$URL_A" "$URL_B" "$COUNT"
echo "next-server CPU over the comparison: $A $(( ($(server_ticks "$A") - beforeA) * 10 )) ms, $B $(( ($(server_ticks "$B") - beforeB) * 10 )) ms"
if [ "$MODE" = trace ]; then
  for v in "$A" "$B"; do node "$HERE/drive.mjs" opt "$WORK/app-$A" "http://127.0.0.1:$(port_of "$v")" 3; done
fi
