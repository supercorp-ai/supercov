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
#
# A <version> is a release published to npm, or `checkout`: this checkout,
# at the version its package.json carries, so that a release commit is
# recorded before it is published. The checkout is packed as npm would
# publish it, and its Linux binary is built in a Rust container
# (SUPERCOV_COMPAT_RUST_IMAGE, cached in the supercov-compat-cargo volume),
# or taken from SUPERCOV_COMPAT_BINARY, a Linux supercov for the container's
# architecture.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
fixture=${1:?usage: scripts/compat-record.sh <fixture> <version>|checkout...}
shift
[ $# -gt 0 ] || { echo "name at least one release, or checkout" >&2; exit 2; }
source_dir=$root/tests/compat/fixtures/$fixture
[ -f "$source_dir/compat.json" ] || { echo "no fixture $fixture" >&2; exit 2; }
image=${SUPERCOV_COMPAT_IMAGE:-node:24-bookworm}
rust_image=${SUPERCOV_COMPAT_RUST_IMAGE:-rust:1.95-bookworm}

# The checkout's Linux binary, built once for every fixture and version.
checkout_binary=
build_checkout() {
  [ -z "$checkout_binary" ] || return 0
  local built
  built=$(mktemp -d)
  if [ -n "${SUPERCOV_COMPAT_BINARY:-}" ]; then
    cp "$SUPERCOV_COMPAT_BINARY" "$built/supercov"
  else
    # The release profile without its whole-program optimisation: the same
    # code, built in minutes rather than most of an hour in a container.
    docker run --rm \
      -v "$root:/src:ro" \
      -v "$built:/out" \
      -v supercov-compat-cargo:/cache \
      -e CARGO_HOME=/cache/home \
      -e CARGO_TARGET_DIR=/cache/target \
      -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
      -e HOST_IDS="$(id -u):$(id -g)" \
      "$rust_image" bash -euo pipefail -c '
        # ruby-prism generates its bindings with libclang.
        apt-get update -qq && apt-get install -y -qq libclang-dev >/dev/null
        cargo build --locked --profile measure -p supercov --manifest-path /src/Cargo.toml
        cp /cache/target/measure/supercov /out/supercov
        chown "$HOST_IDS" /out/supercov
      '
  fi
  chmod +x "$built/supercov"
  checkout_binary=$built
}

for version in "$@"; do
  local_mounts=()
  if [ "$version" = checkout ]; then
    version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$root/package.json")
    [ -n "$version" ] || { echo "package.json carries no version" >&2; exit 2; }
    build_checkout
    local_mounts=(-v "$root:/checkout:ro" -v "$checkout_binary:/checkout-bin:ro" -e FROM_CHECKOUT=1)
    echo "$fixture: recording this checkout as $version"
  fi
  out=$root/tests/compat/runs/$fixture/$version
  staging=$(mktemp -d)
  docker run --rm \
    -v "$source_dir:/fixture:ro" \
    -v "$staging:/out" \
    -v supercov-compat-npm:/root/.npm \
    ${local_mounts[@]+"${local_mounts[@]}"} \
    -e VERSION="$version" \
    -e HOST_IDS="$(id -u):$(id -g)" \
    "$image" bash -euo pipefail -c '
      apt_python() { command -v python3 >/dev/null || { apt-get update -qq && apt-get install -y -qq python3 python3-pip >/dev/null; }; }
      apt_python
      if [ -n "${FROM_CHECKOUT:-}" ]; then
        # The package as npm would publish it, with the checkout'"'"'s binary in
        # place of the native package npm does not have yet.
        mkdir -p /tmp/pack /opt/supercov
        (cd /checkout && npm pack --ignore-scripts --pack-destination /tmp/pack >/dev/null)
        npm install --prefix /opt/supercov --omit=optional --ignore-scripts --no-audit --no-fund /tmp/pack/supercov-$VERSION.tgz >/dev/null
        export SUPERCOV_RUST_BINARY=/checkout-bin/supercov
      fi
      mkdir -p /work && cp -R /fixture /work/project && cd /work/project
      export GIT_AUTHOR_NAME=compat GIT_AUTHOR_EMAIL=compat@example.invalid
      export GIT_COMMITTER_NAME=compat GIT_COMMITTER_EMAIL=compat@example.invalid
      export GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z
      git init -q -b main && git add -A && git commit -qm fixture
      node -e "for (const step of require(\"./compat.json\").setup) console.log(step)" | while read -r step; do bash -c "$step"; done
      if [ -n "${FROM_CHECKOUT:-}" ]; then
        supercov() { node /opt/supercov/node_modules/supercov/bin/supercov.js "$@"; }
      else
        supercov() { npx -y "supercov@$VERSION" "$@"; }
      fi
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
      # Written as root: handed back to whoever runs the script, who copies
      # and removes them (Linux runners keep the container'"'"'s owner).
      chown -R "$HOST_IDS" /out
    '
  rm -rf "$out"
  mkdir -p "$out"
  cp -R "$staging/runs" "$staging/answers" "$out/"
  rm -rf "$staging"
  echo "$fixture $version: $(ls "$out/runs" | tr '\n' ' ')"
done
