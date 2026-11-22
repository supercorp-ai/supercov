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
all_targets="$targets"
module="$(field module)"
sources="$(field sources)"
target_tests="$(field targetTests)"
junit5="$(field junit5)"
testng="$(field testng)"
jdk="$(field jdk)"
gradle_plugin="$(field gradlePlugin)"
export PIT_GRADLE_PLUGIN="${gradle_plugin:-1.15.0}"
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

# A shard (SHARD=k/n) is the k-th of n shares of the project's source files:
# PIT mutates their classes and the strong pass runs their mutants. A
# project's `sources` (a directory) narrows the files to its own, the shard
# or not.
shard_files=()
if [ -n "${SHARD:-}" ] || [ -n "$sources" ]; then
  k="${SHARD%/*}"
  n="${SHARD#*/}"
  [ -n "${SHARD:-}" ] || { k=1; n=1; }
  if [ -n "$sources" ]; then
    mapfile -t all < <(cd "$source" && find "$sources" -maxdepth 1 \( -name '*.java' -o -name '*.kt' \) | sort)
  else
    # A module's own sources only: the tests run are the module's, and PIT
    # mutates only the module's classes.
    mapfile -t all < <(cd "$source" && find "${module:-.}" -path '*/src/main/*' \( -name '*.java' -o -name '*.kt' \) | sed 's|^\./||' | sort)
  fi
  # Shares of about equal size: the largest file first, each to the share
  # with the least so far (a file's mutants go with its size).
  mapfile -t shard_files < <(cd "$source" && python3 - "$k" "$n" "${all[@]}" <<'PY'
import os, sys
k, n, files = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3:]
shares = [[0, []] for _ in range(n)]
for f in sorted(files, key=lambda f: (-os.path.getsize(f), f)):
    share = min(shares, key=lambda s: s[0])
    share[0] += os.path.getsize(f)
    share[1].append(f)
print("\n".join(sorted(shares[k - 1][1])))
PY
)
  targets="$(for f in "${shard_files[@]}"; do
    rel="${f#*/src/main/java/}"; rel="${rel#*/src/main/kotlin/}"; rel="${rel#src/main/java/}"; rel="${rel#src/main/kotlin/}"; rel="${rel#src/main/}"
    class="$(echo "${rel%.*}" | tr / .)"
    echo "$class,$class\$*,${class}Kt,${class}Kt\$*"
  done | paste -sd, -)"
fi

copy="$work/$project-$scenario"
rm -rf "$copy"
cp -r "$source" "$copy"
cd "$copy"

# A surefire the project's old TestNG runs under, where the default refuses
# it: the same for PIT and Supercov.
surefire="$(field surefire)"
if [ -n "$surefire" ]; then
  python3 - pom.xml "$surefire" <<'PY'
import re, sys
p, version = sys.argv[1], sys.argv[2]
s = open(p).read()
plugin = f"<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>{version}</version></plugin>"
m = re.search(r"<build>(.*?)</build>", s, re.S)
body = re.sub(r"<pluginManagement>.*?</pluginManagement>", lambda x: " " * len(x.group(0)), m.group(1), flags=re.S)
i = body.find("<plugins>")
at = m.start(1) + i + len("<plugins>")
s = s[:at] + plugin + s[at:]
open(p, "w").write(s)
PY
fi

now() { date +%s.%N; }
elapsed() { node -e "process.stdout.write(String(Math.round(($2-$1)*1000)))"; }

# Ant, where the project builds with it: the version the build asks for,
# from Apache's archive, and the JUnit jars its build looks for in
# lib/optional.
if [ "$build" = ant ]; then
  ant_version="$(field antVersion)"
  if ! command -v ant > /dev/null || ! ant -version 2>/dev/null | grep -q "$ant_version"; then
    if [ ! -d "$work/apache-ant-$ant_version" ]; then
      curl -fsSL "https://archive.apache.org/dist/ant/binaries/apache-ant-$ant_version-bin.tar.gz" | tar -xz -C "$work"
    fi
    export ANT_HOME="$work/apache-ant-$ant_version"
    export PATH="$ANT_HOME/bin:$PATH"
  fi
  mkdir -p lib/optional
  for artifact in junit:junit:4.13.2 org.hamcrest:hamcrest-core:1.3 org.hamcrest:hamcrest-library:1.3; do
    mvn -B -q dependency:copy -Dartifact="$artifact" -DoutputDirectory=lib/optional > /dev/null
  done
fi

tests_command() {
  if [ "$build" = ant ]; then
    # shellcheck disable=SC2086
    echo ant -noinput $flags "$(field antTarget)"
  elif [ "$build" = maven ]; then
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
  if [ "$build" = ant ]; then
    # PIT's command line over the classes Ant compiled: its Ant task is no
    # more than a launcher for the same.
    # shellcheck disable=SC2086
    ant -noinput -q $flags "$(field antCompile)" >&2
    # PIT's command line with what it depends on (commons-text writes its
    # XML report).
    mkdir -p "$work/pit-cli"
    cat > "$work/pit-cli/pom.xml" <<'POM'
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>benchmark</groupId>
  <artifactId>pit-cli</artifactId>
  <version>1</version>
  <dependencies>
    <dependency>
      <groupId>org.pitest</groupId>
      <artifactId>pitest-command-line</artifactId>
      <version>1.17.1</version>
    </dependency>
  </dependencies>
</project>
POM
    (cd "$work/pit-cli" && mvn -B -q dependency:copy-dependencies -DoutputDirectory=jars > /dev/null)
    mkdir -p build/testcases/tmp
    local classpath
    classpath="$(field antClasspath | tr ',' '\n' | while read -r entry || [ -n "$entry" ]; do
      if [[ "$entry" == *'*'* ]]; then ls "$PWD"/$entry; else echo "$PWD/$entry"; fi
    done | paste -sd, -)"
    local full=false
    [ "$matrix" = 1 ] && full=true
    # shellcheck disable=SC2046
    # The project's class path is PIT's own (its JUnit 4 support looks for
    # JUnit there); given again as --classPath, each test would run twice.
    java -cp "$(ls "$work"/pit-cli/jars/*.jar | paste -sd: -):$(echo "$classpath" | tr , :)" \
      org.pitest.mutationtest.commandline.MutationCoverageReport \
      --reportDir pit-reports --timestampedReports=false --outputFormats XML \
      --targetClasses "$targets" --targetTests "$target_tests" \
      --sourceDirs "$(field antSourceDir)" \
      --excludedTestClasses "$(field excludedTests)" \
      --threads "$threads" --fullMutationMatrix="$full" \
      --timeoutConst 10000 --verbose \
      --jvmArgs "$(field antJvmArgs | sed "s|ROOT|$PWD|g; s|ANTHOME|${ANT_HOME:-}|g")" >&2
    echo pit-reports/mutations.xml
  elif [ "$build" = maven ]; then
    python3 "$here/pit-maven.py" pom.xml "${targets%%,*}"
    # Every target class pattern, where a shard names several.
    python3 - pom.xml "$targets" <<'PY'
import sys
p, targets = sys.argv[1], sys.argv[2]
s = open(p).read()
params = "".join(f"<param>{t}</param>" for t in targets.split(",") if t)
import re
s = re.sub(r"<targetClasses>.*?</targetClasses>", f"<targetClasses>{params}</targetClasses>", s, count=1, flags=re.S)
open(p, "w").write(s)
PY
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
    PIT_MATRIX="$matrix" PIT_THREADS="$threads" PIT_TARGETS="$targets" PIT_TESTS="$all_targets" \
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
    echo "{\"project\":\"$project\",\"scenario\":\"$scenario\",\"shard\":\"${SHARD:-}\",\"ms\":$(elapsed "$started" "$ended"),\"threads\":$threads}" > "$results/$scenario.json"
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
    file_options=()
    for f in "${shard_files[@]}"; do file_options+=(--file "$f"); done
    SUPERCOV_STRONG_JVMS="$threads" "$supercov" runs latest mutants test --json "${file_options[@]}" -- $command \
      > "$results/supercov-strong.json" 2> "$results/supercov-strong.log" || true
    ended="$(now)"
    cp .supercov/runs/*/strong-mutation.json "$results/" 2>/dev/null || true
    mkdir -p "$results/strong"
    cp -r .supercov/strong/* "$results/strong/" 2>/dev/null || true
    if [ -f "$results/pit-matrix.xml" ]; then
      "$supercov" runs latest mutants pit "$results/pit-matrix.xml" --json "${file_options[@]}" > "$results/supercov-pit.json" || true
      "$supercov" runs latest mutants pit "$results/pit-matrix.xml" "${file_options[@]}" > "$results/supercov-pit.txt" || true
    fi
    echo "{\"project\":\"$project\",\"scenario\":\"supercov\",\"measureMs\":$(elapsed "$started" "$measured"),\"strongMs\":$(elapsed "$measured" "$ended"),\"threads\":$threads}" > "$results/supercov.json"
    ;;
  handcheck)
    # A disagreement checked by hand: the mutant applied to the source
    # (HANDCHECK: file:line:from:to, the line's text replaced), then the test
    # PIT or Supercov named run alone, and the whole suite.
    IFS=: read -r file line from to <<< "$HANDCHECK"
    python3 - "$file" "$line" "$from" "$to" <<'PY'
import sys
path, line, old, new = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
lines = open(path).read().split("\n")
assert old in lines[line - 1], lines[line - 1]
lines[line - 1] = lines[line - 1].replace(old, new, 1)
open(path, "w").write("\n".join(lines))
PY
    {
      echo "== $HANDCHECK"
      echo "== alone: $HANDCHECK_TEST"
      if [ "$build" = maven ]; then
        # shellcheck disable=SC2086
        mvn -B -q $flags -Dtest="$HANDCHECK_TEST" -Dsurefire.failIfNoSpecifiedTests=false test && echo "PASSED alone" || echo "FAILED alone"
      else
        task="test"
        [ -n "$module" ] && task=":$module:test"
        # shellcheck disable=SC2086
        ./gradlew --no-daemon -q $flags "$task" --tests "$HANDCHECK_TEST" && echo "PASSED alone" || echo "FAILED alone"
      fi
      echo "== whole suite"
      # shellcheck disable=SC2086
      $(tests_command) && echo "PASSED whole" || echo "FAILED whole"
    } > "$results/handcheck-$(echo "$HANDCHECK" | tr '/:' '__').log" 2>&1
    ;;
  *)
    echo "unknown scenario $scenario" >&2
    exit 2
    ;;
esac
