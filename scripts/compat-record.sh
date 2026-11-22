#!/usr/bin/env bash
# Records a released Supercov's runs of a compatibility fixture, for the
# backward-compatibility suite (tests/compat/README.md).
#
#   scripts/compat-record.sh <fixture> <version>...
#
# Each release measures tests/compat/fixtures/<fixture> in a Linux container,
# at a fixed path and commit, three times: the whole suite and each of its
# two shards. What it stores, and what it answered about each run, lands in
# tests/compat/runs/<fixture>/<version>/.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
fixture=${1:?usage: scripts/compat-record.sh <fixture> <version>...}
shift
[ $# -gt 0 ] || { echo "name at least one release" >&2; exit 2; }
source_dir=$root/tests/compat/fixtures/$fixture
[ -f "$source_dir/compat.json" ] || { echo "no fixture $fixture" >&2; exit 2; }
image=${SUPERCOV_COMPAT_IMAGE:-node:24-bookworm}

for version in "$@"; do
  out=$root/tests/compat/runs/$fixture/$version
  staging=$(mktemp -d)
  docker run --rm \
    -v "$source_dir:/fixture:ro" \
    -v "$staging:/out" \
    -v supercov-compat-npm:/root/.npm \
    -e VERSION="$version" \
    "$image" bash -euo pipefail -c '
      apt_python() { command -v python3 >/dev/null || { apt-get update -qq && apt-get install -y -qq python3 python3-pip >/dev/null; }; }
      apt_python
      mkdir -p /work && cp -R /fixture /work/project && cd /work/project
      export GIT_AUTHOR_NAME=compat GIT_AUTHOR_EMAIL=compat@example.invalid
      export GIT_COMMITTER_NAME=compat GIT_COMMITTER_EMAIL=compat@example.invalid
      export GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z
      git init -q -b main && git add -A && git commit -qm fixture
      node -e "for (const step of require(\"./compat.json\").setup) console.log(step)" | while read -r step; do bash -c "$step"; done
      supercov() { npx -y "supercov@$VERSION" "$@"; }
      record() {
        local name=$1; shift
        supercov -- "$@" >"/out/$name.log" 2>&1
        local id
        id=$(ls -t .supercov/runs 2>/dev/null | head -1)
        [ -n "$id" ] || { echo "$VERSION recorded no run of $name" >&2; cat "/out/$name.log" >&2; exit 1; }
        mkdir -p "/out/runs/$name" "/out/answers/$name"
        cp -R ".supercov/runs/$id/." "/out/runs/$name/"
        # One stale query index is enough to show it is not trusted.
        [ "$name" = full ] || rm -f "/out/runs/$name"/query-index*
        supercov runs "$id" >"/out/answers/$name/summary.txt" 2>&1 || true
        echo "$id"
      }
      readarray -t full < <(node -e "for (const a of require(\"./compat.json\").command) console.log(a)")
      readarray -t one < <(node -e "for (const a of require(\"./compat.json\").shards[0]) console.log(a)")
      readarray -t two < <(node -e "for (const a of require(\"./compat.json\").shards[1]) console.log(a)")
      file=$(node -e "console.log(require(\"./compat.json\").file)")
      test=$(node -e "console.log(require(\"./compat.json\").test)")
      full_id=$(record full "${full[@]}")
      supercov runs "$full_id" files >/out/answers/full/files.txt 2>&1 || true
      supercov runs "$full_id" gaps >/out/answers/full/gaps.txt 2>&1 || true
      supercov runs "$full_id" file "$file" >/out/answers/full/file.txt 2>&1 || true
      supercov runs "$full_id" test "$test" >/out/answers/full/test.txt 2>&1 || true
      one_id=$(record shard-1 "${one[@]}")
      two_id=$(record shard-2 "${two[@]}")
      mkdir -p /out/answers/merge
      supercov merge "$one_id" "$two_id" >/out/answers/merge/merge.txt 2>&1 || true
      merged=$(grep -oE "merged run run_[0-9a-f]+" /out/answers/merge/merge.txt | cut -d" " -f3 || true)
      [ -z "$merged" ] || supercov runs "$merged" >/out/answers/merge/summary.txt 2>&1 || true
      supercov --version >/out/version.txt 2>&1 || true
    '
  rm -rf "$out"
  mkdir -p "$out"
  cp -R "$staging/runs" "$staging/answers" "$out/"
  rm -rf "$staging"
  echo "$fixture $version: $(ls "$out/runs" | tr '\n' ' ')"
done
