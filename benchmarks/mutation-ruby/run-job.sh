#!/usr/bin/env bash
# One project of the Ruby mutation benchmark: the mutant gem's verdicts on
# every test per mutant, Supercov's strong pass on the same project, their
# times, and the join of the two.
#
#   benchmarks/mutation-ruby/run-job.sh <project> <out-dir>
#
# Needs Ruby 3.4 with Bundler, git, and SUPERCOV_BIN (the supercov binary).
# Jobs: SUPERCOV_STRONG_JOBS and mutant's --jobs are both JOBS (default:
# the machine's CPUs), so the two run as wide.
set -euo pipefail

project="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
jobs="${JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
work="$(mktemp -d)"

# name repo tag integration subjects require test-command...
case "$project" in
  dry-inflector)
    repo=dry-rb/dry-inflector tag=v1.3.1 integration=rspec subjects='Dry::Inflector*' requires=(dry/inflector)
    command=(bundle exec rspec) dependencies=() ;;
  dotenv)
    repo=bkeepers/dotenv tag=v3.2.0 integration=rspec subjects='Dotenv*' requires=(dotenv)
    command=(bundle exec rspec) ;;
  erubi)
    repo=jeremyevans/erubi tag=1.13.1 integration=minitest subjects='Erubi*' requires=(erubi ./test/test.rb)
    dependencies=('"minitest", "~> 5.25"' '"minitest-global_expectations"')
    command=(bundle exec ruby -Ilib test/test.rb) ;;
  *) echo "unknown project $project" >&2; exit 2 ;;
esac

git clone -q --depth 1 --branch "$tag" "https://github.com/$repo.git" "$work/$project"
cd "$work/$project"
git -c advice.detachedHead=false checkout -q "$tag"

# mutant and its integration in the project's bundle, and every test made a
# candidate for every subject: mutant otherwise picks tests by their names.
if [ ! -f Gemfile ]; then
  # Without `gemspec`: erubi's gemspec requires the library itself, which
  # Bundler would load before anything else.
  printf 'source "https://rubygems.org"\n' > Gemfile
  for dependency in "${dependencies[@]}"; do printf 'gem %s\n' "$dependency" >> Gemfile; done
fi
printf '\ngem "mutant", "0.17.1"\ngem "mutant-%s", "0.17.1"\n' "$integration" >> Gemfile
if [ "$integration" = rspec ]; then
  printf '\nRSpec.configure { |config| config.define_derived_metadata { |metadata| metadata[:mutant_expression] = "%s" } }\n' "$subjects" >> spec/spec_helper.rb
  printf 'gem "rspec"\n' >> Gemfile
else
  mkdir -p "$work/shim"
  # The integration first: it turns `Minitest.autorun` off before a test
  # file's `require "minitest/autorun"` would run the suite in every fork.
  printf 'require "mutant"\nrequire "mutant/integration/minitest"\nMinitest::Test.cover("%s")\n' "$subjects" > "$work/shim/every_test.rb"
  requires=("$work/shim/every_test.rb" "${requires[@]}")
fi
bundle config set --local path "$work/bundle" >/dev/null
bundle install --jobs 8 --quiet

require_flags=()
for name in "${requires[@]}"; do require_flags+=(--require "$name"); done

seconds() { date +%s.%N; }

# The mutant gem, every test per mutant.
start=$(seconds)
timeout "${MUTANT_TIMEOUT:-300}" bundle exec mutant run --usage opensource --integration "$integration" --include lib \
  "${require_flags[@]}" --jobs "$jobs" --mutation-timeout 60 "$subjects" > "$out/mutant.log" 2>&1 || true
mutant_seconds=$(echo "$(seconds) - $start" | bc)
cp .mutant/results/*.json.gz "$out/" 2>/dev/null || true

# Supercov: the measured run, then the strong pass.
start=$(seconds)
timeout 120 "$SUPERCOV_BIN" -- "${command[@]}" > "$out/supercov-run.log" 2>&1
measured_seconds=$(echo "$(seconds) - $start" | bc)
start=$(seconds)
SUPERCOV_STRONG_JOBS="$jobs" timeout "${STRONG_TIMEOUT:-240}" "$SUPERCOV_BIN" runs latest mutants test --json > "$out/supercov-strong.json" 2> "$out/supercov-strong.log" || true
strong_seconds=$(echo "$(seconds) - $start" | bc)

"$SUPERCOV_BIN" runs latest mutants --stryker .mutant/results --json --limit 0 > "$out/join.json" 2> "$out/join.log" || true
"$SUPERCOV_BIN" runs latest mutants --stryker .mutant/results --format jsonl --output "$out/join.jsonl" 2>> "$out/join.log" || true
ruby "$here/summarize.rb" "$out/join.json" "$out/join.jsonl" "$project" "$mutant_seconds" "$measured_seconds" "$strong_seconds" "$jobs" > "$out/summary.json"
cat "$out/summary.json"
