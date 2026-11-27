#!/usr/bin/env bash
# One project of the Ruby mutation benchmark whose runner the mutant gem has
# no integration for (test-unit, Cucumber): Supercov's strong pass, then
# every mutant it ran run again by hand (by-hand.rb), the whole suite per
# mutant in a fresh process.
#
#   benchmarks/mutation-ruby/by-hand-job.sh <project> <out-dir>
#
# Needs Ruby 3.4 with Bundler, git, and SUPERCOV_BIN (the supercov binary).
# The weak verdicts come from a run with the value probes; a weak verdict of
# not infected that the suite kills by hand is a soundness violation.
set -euo pipefail

project="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
jobs="${JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
work="$(mktemp -d)"

case "$project" in
  ostruct)
    repo=ruby/ostruct tag=v0.6.3
    command=(ruby -Ilib -Itest/lib -rhelper -e 'Dir["test/**/test_*.rb"].sort.each { |file| require File.expand_path(file) }') ;;
  json_test_data)
    repo=oscaralanpierce/json_test_data tag=1.2.0
    command=(bundle exec cucumber --publish-quiet) ;;
  *) echo "unknown project $project" >&2; exit 2 ;;
esac

git clone -q --depth 1 --branch "$tag" "https://github.com/$repo.git" "$work/$project"
cd "$work/$project"

case "$project" in
  ostruct)
    # test-unit and the core assertions its helper includes.
    export GEM_HOME="$work/gems" GEM_PATH="$work/gems:$(ruby -e 'print Gem.default_dir')"
    export PATH="$GEM_HOME/bin:$PATH"
    gem install --no-document --silent test-unit test-unit-ruby-core ;;
  json_test_data)
    # Today's Cucumber in place of the 2.x the gemspec asks for, and the
    # scenarios' random data seeded, so a mutant meets the same data in the
    # strong pass and by hand.
    printf 'source "https://rubygems.org"\ngem "regexp-examples"\ngem "json-schema"\ngem "rspec-expectations"\ngem "cucumber"\n' > Gemfile
    rm -f Gemfile.lock
    printf 'Before { srand(1) }\n' > features/support/seed.rb
    # Coveralls, which the support file loads for its own coverage, is not
    # in the bundle.
    sed -i.orig '/overalls/d' features/support/env.rb && rm features/support/env.rb.orig
    bundle config set --local path "$work/bundle" >/dev/null
    bundle install --jobs 8 --quiet ;;
esac

seconds() { date +%s.%N; }
median() { sort -n | awk '{ value[NR] = $1 } END { print value[int((NR + 1) / 2)] }'; }
time_runs() {
  for _ in 1 2 3 4 5; do
    local from; from=$(seconds)
    "$@" > /dev/null 2>&1 || true
    echo "$(seconds) - $from" | bc
  done | median
}
plain_seconds=$(time_runs timeout 120 "${command[@]}")
unprobed_seconds=$(time_runs timeout 120 "$SUPERCOV_BIN" -- "${command[@]}")
probed_seconds=$(time_runs timeout 120 env SUPERCOV_MUTANTS=1 "$SUPERCOV_BIN" -- "${command[@]}")
# Supercov's own split of one run each: the test command's share (`tests=`)
# apart from setting up and publishing.
timeout 120 "$SUPERCOV_BIN" -- "${command[@]}" > "$out/unprobed.log" 2>&1 || true
SUPERCOV_MUTANTS=1 timeout 120 "$SUPERCOV_BIN" -- "${command[@]}" > "$out/probed.log" 2>&1 || true

# The weak verdicts, from a run with the value probes.
SUPERCOV_MUTANTS=1 timeout 120 "$SUPERCOV_BIN" -- "${command[@]}" > "$out/supercov-probed-run.log" 2>&1
"$SUPERCOV_BIN" runs latest mutants --format jsonl --output "$out/mutants.jsonl" 2> "$out/mutants.log" || true
# The strong pass on a run without them, where no weak verdict decides a
# mutant: every mutant a test reached runs, and is run again by hand.
start=$(seconds)
timeout 120 "$SUPERCOV_BIN" -- "${command[@]}" > "$out/supercov-run.log" 2>&1
measured_seconds=$(echo "$(seconds) - $start" | bc)
start=$(seconds)
SUPERCOV_STRONG_JOBS="$jobs" timeout "${STRONG_TIMEOUT:-240}" "$SUPERCOV_BIN" runs latest mutants test --json > "$out/supercov-strong.json" 2> "$out/supercov-strong.log" || true
strong_seconds=$(echo "$(seconds) - $start" | bc)

start=$(seconds)
timeout "${BY_HAND_TIMEOUT:-300}" ruby "$here/by-hand.rb" . "$jobs" -- "${command[@]}" > "$out/by-hand.json" 2> "$out/by-hand.log" || true
by_hand_seconds=$(echo "$(seconds) - $start" | bc)

ruby -rjson -e '
  by_hand = JSON.parse(File.read(ARGV[0])) rescue { "error" => File.read(ARGV[1]) }
  strong = JSON.parse(File.read(ARGV[2]))["summary"] rescue nil
  mutants = File.readlines(ARGV[3]).map { |line| JSON.parse(line) } rescue []
  weak = mutants.group_by { |mutant| mutant["status"] }.transform_values(&:size)
  # A mutant the weak verdict calls not infected or not reached that the
  # suite killed by hand.
  killed = (by_hand["killed"] || []).to_h { |id| [id, true] }
  violations = mutants.select { |mutant| %w[not-infected not-reached].include?(mutant["status"]) && killed[mutant["id"]] }.map { |mutant| mutant["id"] }
  by_hand.delete("killed")
  names = %w[project measured strong byHand plain unprobed probed]
  values = ARGV[4..]
  seconds = names.drop(1).zip(values.drop(1).map(&:to_f)).to_h
  tests_ms = ->(log) { File.read(log)[/timings .*tests=([\d.]+)ms/, 1]&.to_f rescue nil }
  seconds["testsMs"] = { unprobed: tests_ms.call(File.join(File.dirname(ARGV[0]), "unprobed.log")), probed: tests_ms.call(File.join(File.dirname(ARGV[0]), "probed.log")) }
  puts JSON.pretty_generate(project: values[0], strong: strong, weak: weak, weakSoundnessViolations: violations, byHand: by_hand, seconds: seconds)
' "$out/by-hand.json" "$out/by-hand.log" "$out/supercov-strong.json" "$out/mutants.jsonl" \
  "$project" "$measured_seconds" "$strong_seconds" "$by_hand_seconds" "$plain_seconds" "$unprobed_seconds" "$probed_seconds" > "$out/summary.json"
cat "$out/summary.json"
