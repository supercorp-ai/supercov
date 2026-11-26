#!/usr/bin/env bash
# The fast tier's checks on this machine, before a push: what CI's `Fast
# tier` status will say, in minutes instead of a round trip.
#
#   scripts/pre-push.sh [--base <ref>] [--coverage] [--no-tests]
#   npm run pre-push -- [--coverage]
#
#   1. `npm run check:lint`: the fault-site check, `cargo fmt --check` and
#      clippy with warnings denied, as CI's lint job runs them.
#   2. The affected tests, picked as CI's plan picks them: Supercov's own
#      `tests affected` over the map main's Self Coverage keeps (downloaded
#      once per map, with `gh`, into target/pre-push), then
#      `scripts/ci-affected.mjs` for the plan and the nextest filter, and
#      `cargo nextest run` of them. Without `gh` or a map, the quick check's
#      selection (scripts/quick-check.mjs) stands in, and it says so.
#   3. With --coverage, the slow tier's gate on this machine: the tests Self
#      Coverage would measure for the change, measured by this commit's
#      Supercov, and every line the change adds or changes held to 100%
#      lines, branches and MC/DC (`ci-affected.mjs changed-gaps`). It builds
#      the instrumented workspace, so it takes longer; macOS measures a
#      handful of platform lines Linux does not, and the reverse.
#
# It checks what is committed: the map is asked about HEAD against the merge
# base with <base> (origin/main), as CI asks about the pull request.
set -euo pipefail

base=origin/main
coverage=false
tests=true
while [ $# -gt 0 ]; do
  case "$1" in
    --base) base=$2; shift 2 ;;
    --coverage) coverage=true; shift ;;
    --no-tests) tests=false; shift ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "pre-push: unknown option $1" >&2; exit 2 ;;
  esac
done

root=$(git rev-parse --show-toplevel)
cd "$root"
work="$root/target/pre-push"
mkdir -p "$work"
started=$(date +%s)
step() { printf '\n== %s (%ss)\n' "$1" "$(( $(date +%s) - started ))"; }

git fetch -q origin "${base#origin/}" 2>/dev/null || echo "pre-push: could not fetch $base; using the local copy"
merge_base=$(git merge-base "$base" HEAD)
git diff --name-only "$merge_base" HEAD > "$work/changed.txt"
echo "pre-push: $(wc -l < "$work/changed.txt" | tr -d ' ') file(s) changed since $(git rev-parse --short "$merge_base") ($base)"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "pre-push: uncommitted changes are not checked: commit them first to check what you push"
fi

step "lint: fault sites, cargo fmt, clippy"
npm run --silent check:lint

[ "$tests" = true ] || { step "done"; exit 0; }

# ------------------------------------------------------------------ the map

# The newest Self Coverage run of main whose commit the merge base holds,
# with its map kept, as .github/actions/affected finds it.
find_map() {
  command -v gh > /dev/null 2>&1 || { echo "no gh"; return 1; }
  local repo id sha
  repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null) || { echo "gh cannot see the repository"; return 1; }
  while read -r id sha; do
    [ -n "$id" ] || continue
    git merge-base --is-ancestor "$sha" "$merge_base" 2>/dev/null || continue
    if gh api "repos/$repo/actions/runs/$id/artifacts?name=affected-map" --jq '.artifacts[] | select(.expired | not) | .id' | grep -q .; then
      echo "$repo $id $sha"
      return 0
    fi
  done < <(gh run list --repo "$repo" --workflow self-coverage.yml --branch main --limit 40 \
    --json databaseId,headSha,status --jq '.[] | select(.status == "completed") | "\(.databaseId) \(.headSha)"')
  echo "no Self Coverage map of a commit this branch descends from"
  return 1
}

# The tests CI's plan picks, into $work/selection-<workflow>.json; returns 1
# when no map can answer.
plan_from_map() {
  local found repo id sha map tree supercov answer
  found=$(find_map) || { echo "pre-push: $found"; return 1; }
  read -r repo id sha <<< "$found"
  map="$work/map-$id"
  if [ ! -f "$map/merged-id" ]; then
    echo "pre-push: downloading the map of Self Coverage run $id ($sha), once (about 1.5 GB)"
    rm -rf "$work"/map-*
    mkdir -p "$map"
    gh run download "$id" --repo "$repo" -n affected-map -D "$map"
    gh run download "$id" --repo "$repo" -n test-seconds -D "$map" 2> /dev/null || true
  fi
  # A checkout of HEAD beside this one, holding the map's runs, as CI's
  # checkout holds them: the map reads the committed files, and this
  # checkout's own .supercov is left alone.
  tree="$work/tree"
  if [ ! -d "$tree/.git" ] && [ ! -f "$tree/.git" ]; then
    git worktree add -q --detach "$tree" HEAD
  else
    git -C "$tree" checkout -q --detach HEAD
  fi
  if [ "$(cat "$tree/.supercov/map-id" 2> /dev/null)" != "$id" ]; then
    rm -rf "$tree/.supercov"
    mkdir -p "$tree/.supercov"
    tar -xzf "$map/map.tar.gz" -C "$tree"
    echo "$id" > "$tree/.supercov/map-id"
  fi
  # The map's own Supercov where it runs here; this commit's elsewhere.
  if [ "$(uname -s)-$(uname -m)" = Linux-x86_64 ]; then
    supercov="$map/supercov"
    chmod +x "$supercov"
  else
    step "build Supercov to read the map (cargo build --profile measure)"
    cargo build -q --profile measure -p supercov
    supercov="$root/target/measure/supercov"
  fi
  step "ask the map which tests the change can affect"
  answer="$work/answer"
  rm -rf "$answer"
  mkdir -p "$answer"
  run=$(cat "$map/merged-id")
  ( cd "$tree" && "$supercov" runs "$run" tests affected --json > "$answer/affected.json" ) &
  ( cd "$tree" && { "$supercov" runs "$run" tests affected --names > "$answer/names.txt" || rm -f "$answer/names.txt"; } ) &
  ( cd "$tree" && { "$supercov" runs "$run" tests affected --files > "$answer/files.txt" || rm -f "$answer/files.txt"; } ) &
  wait || true
  if [ -f "$map/test-seconds.tsv" ]; then export CI_TEST_SECONDS="$map/test-seconds.tsv"; fi
  # CI's plan, given the event a push to a pull request is.
  local branch
  branch=$(git rev-parse --abbrev-ref HEAD)
  printf '{"action":"synchronize","pull_request":{"title":"%s","head":{"ref":"%s"},"labels":[]}}\n' \
    "$(git log -1 --format=%s | sed 's/["\\]/ /g')" "$branch" > "$work/event.json"
  for workflow in ci self-coverage; do
    ( cd "$tree" && GITHUB_EVENT_NAME=pull_request GITHUB_EVENT_PATH="$work/event.json" CHANGED="$work/changed.txt" \
        GITHUB_OUTPUT="$work/plan-$workflow.out" GITHUB_STEP_SUMMARY="$work/plan-$workflow.md" \
        node scripts/ci-affected.mjs plan "$workflow" "$answer" > /dev/null \
      && mv selection.json "$work/selection-$workflow.json" )
  done
  sed -n 's/^jobs=//p' "$work/plan-ci.out" > "$work/jobs-ci.json"
  sed -n 's/^jobs=//p' "$work/plan-self-coverage.out" > "$work/jobs-self-coverage.json"
  return 0
}

: > "$work/plan-ci.out"
rm -f "$work/plan-ci.md"
if plan_from_map; then
  grep -E '^\| (check|engine-integrations|rust-|launch-|bun-|kotlin-|lint|clang-)' "$work/plan-ci.md" | sed 's/^/  /' || true
  check=$(node -e 'const j = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).check; console.log(j.run ? j.mode : "skip")' "$work/jobs-ci.json")
  step "affected tests (cargo nextest, as CI's check job: $check)"
  if [ "$check" = skip ]; then
    echo "pre-push: CI's plan runs none of the workspace's tests for this change"
  else
    cargo nextest list --workspace --message-format json > "$work/tests.json"
    all=""
    [ "$check" = full ] && all="--all" && echo "pre-push: CI runs every test for this change (it changes how CI runs, or the map cannot answer): so does this"
    NEXTEST_CONFIG_OUT="$work/nextest.toml" node scripts/ci-affected.mjs nextest "$work/selection-ci.json" "$work/tests.json" 1 1 $all
    cargo nextest run --workspace --config-file "$work/nextest.toml" --profile affected --no-fail-fast --no-tests=pass
  fi
  echo "pre-push: CI also runs the jobs the table above says run (engine integrations, wasm, workers); this runs the workspace's tests alone"
else
  step "affected tests: the quick check's selection, as no map could answer"
  node scripts/quick-check.mjs --base "$base" --run
fi

# --------------------------------------------------------- changed lines

if [ "$coverage" = true ]; then
  [ -f "$work/selection-self-coverage.json" ] || { echo "pre-push: --coverage needs the map, to know which tests Self Coverage measures" >&2; exit 1; }
  step "changed-line coverage: build the measuring Supercov"
  cargo build -q --profile measure -p supercov
  names=$(node scripts/ci-affected.mjs cargo-test "$work/selection-self-coverage.json" 0 1)
  # What Self Coverage leaves out on this platform, as its slices do.
  if [ "$(uname -s)" = Darwin ]; then
    elsewhere="docker_servers docker_clients rust_region_container rust_runtime::tests::a_region_in_a_container_is_read_live_from_the_host every_file_of_the_corpus_parses_or_is_known_not_to"
  else
    elsewhere="clang_mcdc_oracle every_file_of_the_corpus_parses_or_is_known_not_to"
  fi
  names=$(printf '%s\n' $names | grep -vxF -f <(printf '%s\n' $elsewhere) | tr '\n' ' ' || true)
  if [ -z "${names// }" ] || [ "$names" = "no_test_was_dealt_to_this_slice " ]; then
    echo "pre-push: Self Coverage measures no test for this change"
  else
    step "changed-line coverage: measure $(echo $names | wc -w | tr -d ' ') test(s)"
    log="$work/measure.log"
    # shellcheck disable=SC2086
    "$root/target/measure/supercov" -- cargo test --workspace -- --include-ignored --exact $names 2>&1 | tee "$log" || true
    run=$(grep -oE 'runs/run_[0-9a-f]{16}' "$log" | tail -1 | sed 's#runs/##')
    [ -n "$run" ] || { echo "pre-push: the measurement left no run" >&2; exit 1; }
    for metric in lines branches mcdc; do
      "$root/target/measure/supercov" runs "$run" gaps --format jsonl --metric "$metric" --limit 0 --output "$work/$metric-gaps.jsonl"
    done
    step "changed-line coverage: the lines this branch adds or changes"
    node scripts/ci-affected.mjs changed-gaps "$merge_base" "$work/lines-gaps.jsonl" "$work/branches-gaps.jsonl" "$work/mcdc-gaps.jsonl"
  fi
fi

step "done: the fast tier's checks pass here"
