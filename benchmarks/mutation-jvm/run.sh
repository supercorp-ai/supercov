#!/usr/bin/env bash
# The JVM mutation benchmark: one project, one scenario.
#
#   benchmarks/mutation-jvm/run.sh <project> <scenario> <results-dir>
#
# Scenarios:
#   pit-matrix  PIT 1.17.1 with fullMutationMatrix (every test against every
#               mutant): the ground truth the strong pass is compared with.
#   pit         PIT as a team runs it (stops at the first kill): the time
#               the strong pass is measured against.
#   supercov    `supercov -- <tests>`, `supercov runs latest mutants test`,
#               and, given PIT's matrix in <results-dir>/pit-matrix.xml,
#               `supercov runs latest mutants pit`.
#
# Projects are in projects.json, cloned shallow at their tag. SUPERCOV is the
# binary to run; THREADS how many threads PIT and the strong pass use.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
project="$1"
scenario="$2"
results="$(mkdir -p "$3" && cd "$3" && pwd)"
work="${WORK:-$PWD/.mutation-jvm}"
threads="${THREADS:-$(nproc)}"
supercov="${SUPERCOV:-supercov}"

field() {
  node -e "const p=require('$here/projects.json')['$project']; const v=p['$1']; process.stdout.write(v===undefined?'':String(v))"
}
repository="$(field repository)"
tag="$(field tag)"
build="$(field build)"
flags="$(field flags)"
targets="$(field targets)"
module="$(field module)"
junit5="$(field junit5)"
testng="$(field testng)"
jdk="$(field jdk)"
# The JDK the project builds with: setup-java leaves each one it installed
# in JAVA_HOME_<version>_X64.
jdk_home="JAVA_HOME_${jdk}_X64"
if [ -n "$jdk" ] && [ -n "${!jdk_home:-}" ]; then
  export JAVA_HOME="${!jdk_home}"
  export PATH="$JAVA_HOME/bin:$PATH"
fi

mkdir -p "$work"
source="$work/$project"
if [ ! -d "$source" ]; then
  git clone -q --depth 1 --branch "$tag" "$repository" "$source"
fi

copy="$work/$project-$scenario"
rm -rf "$copy"
cp -r "$source" "$copy"
cd "$copy"

now() { date +%s.%N; }
elapsed() { node -e "process.stdout.write(String(Math.round(($2-$1)*1000)))"; }

tests_command() {
  if [ "$build" = maven ]; then
    # shellcheck disable=SC2086
    echo mvn -B -q $flags test
  else
    local task="test"
    [ -n "$module" ] && task=":$module:test"
    # shellcheck disable=SC2086
    echo ./gradlew --no-daemon -q $flags "$task"
  fi
}

run_pit() {
  local matrix="$1"
  if [ "$build" = maven ]; then
    python3 "$here/pit-maven.py" pom.xml "$targets"
    if [ "$matrix" = 0 ]; then
      sed -i 's|<fullMutationMatrix>true</fullMutationMatrix>|<fullMutationMatrix>false</fullMutationMatrix>|' pom.xml
    fi
    sed -i "s|<threads>4</threads>|<threads>$threads</threads>|" pom.xml
    if [ "$testng" = true ]; then
      sed -i 's|<artifactId>pitest-junit5-plugin</artifactId>|<artifactId>pitest-testng-plugin</artifactId>|; s|<version>1.2.1</version>|<version>1.0.0</version>|' pom.xml
    elif [ "$junit5" != true ]; then
      python3 - pom.xml <<'PY'
import re, sys
p = sys.argv[1]; s = open(p).read()
s = re.sub(r'<dependencies>\s*<dependency>\s*<groupId>org.pitest</groupId>\s*<artifactId>pitest-junit5-plugin</artifactId>.*?</dependencies>', '', s, flags=re.S)
open(p, 'w').write(s)
PY
    fi
    # shellcheck disable=SC2086
    mvn -B -q $flags test-compile org.pitest:pitest-maven:mutationCoverage >&2
    find . -path '*pit-reports*' -name mutations.xml | head -1
  else
    local task="pitest"
    [ -n "$module" ] && task=":$module:pitest"
    PIT_MATRIX="$matrix" PIT_THREADS="$threads" PIT_TARGETS="$targets" \
      PIT_JUNIT5="$([ "$junit5" = true ] && echo 1 || echo 0)" \
      PIT_TESTNG="$([ "$testng" = true ] && echo 1 || echo 0)" \
      ./gradlew --no-daemon -q $flags --init-script "$here/pit-init.gradle" "$task" >&2
    find . -path '*reports/pitest*' -name mutations.xml | head -1
  fi
}

case "$scenario" in
  pit-matrix | pit)
    matrix=1
    [ "$scenario" = pit ] && matrix=0
    started="$(now)"
    report="$(run_pit "$matrix")"
    ended="$(now)"
    cp "$report" "$results/$scenario.xml"
    echo "{\"project\":\"$project\",\"scenario\":\"$scenario\",\"ms\":$(elapsed "$started" "$ended"),\"threads\":$threads}" > "$results/$scenario.json"
    ;;
  supercov)
    command="$(tests_command)"
    started="$(now)"
    # shellcheck disable=SC2086
    # Measured with the mutants listed and their values recorded, as
    # `supercov mutants` measures.
    SUPERCOV_MUTANTS=1 "$supercov" -- $command > "$results/supercov-run.log" 2>&1 || true
    measured="$(now)"
    # shellcheck disable=SC2086
    SUPERCOV_STRONG_JVMS="$threads" "$supercov" runs latest mutants test --json -- $command \
      > "$results/supercov-strong.json" 2> "$results/supercov-strong.log" || true
    ended="$(now)"
    cp .supercov/runs/*/strong-mutation.json "$results/" 2>/dev/null || true
    mkdir -p "$results/strong"
    cp -r .supercov/strong/* "$results/strong/" 2>/dev/null || true
    if [ -f "$results/pit-matrix.xml" ]; then
      "$supercov" runs latest mutants pit "$results/pit-matrix.xml" --json > "$results/supercov-pit.json" || true
      "$supercov" runs latest mutants pit "$results/pit-matrix.xml" > "$results/supercov-pit.txt" || true
    fi
    echo "{\"project\":\"$project\",\"scenario\":\"supercov\",\"measureMs\":$(elapsed "$started" "$measured"),\"strongMs\":$(elapsed "$measured" "$ended"),\"threads\":$threads}" > "$results/supercov.json"
    ;;
  *)
    echo "unknown scenario $scenario" >&2
    exit 2
    ;;
esac
