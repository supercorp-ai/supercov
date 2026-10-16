// Every construct the JVM instrumenter rewrites, measured end to end.
//
//   node scripts/java-syntax-zoo.mjs
//
// A small Maven project whose tests drive each construct both ways: switch
// expressions with `yield`, `instanceof` patterns, records, the enhanced for,
// do/while, labelled break and continue, try/catch/finally and
// try-with-resources, lambdas and streams with conditions, ternaries and
// compound conditions. Each class is read back for what its tests showed: a
// construct measured wrongly reads as uncovered here.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-java-syntax-zoo-'));
const root = resolve(temporary, 'zoo');
const homebrewJdk = '/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home';
const javaHome = process.env.JAVA_HOME ?? (existsSync(homebrewJdk) ? homebrewJdk : undefined);

const files = {
  'pom.xml': `<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>zoo</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
      </plugin>
    </plugins>
  </build>
</project>
`,
  'src/main/java/zoo/Flow.java': `package zoo;

import java.io.StringReader;
import java.io.BufferedReader;
import java.io.IOException;
import java.util.List;
import java.util.stream.Collectors;

public class Flow {
    public record Point(int x, int y) {
        public boolean onAxis() {
            return x == 0 || y == 0;
        }
    }

    public static String grade(int score) {
        return switch (score / 10) {
            case 10, 9 -> "A";
            case 8 -> {
                String grade = score % 10 >= 5 ? "B+" : "B";
                yield grade;
            }
            default -> "F";
        };
    }

    public static String describe(Object value) {
        if (value instanceof String text && !text.isEmpty()) {
            return "text " + text;
        }
        if (value instanceof Point point) {
            return point.onAxis() ? "axis" : "point";
        }
        return "other";
    }

    public static int sumUntil(List<Integer> values, int stop) {
        int total = 0;
        for (int value : values) {
            if (value == stop) {
                break;
            }
            total += value;
        }
        return total;
    }

    public static int steps(int n) {
        int steps = 0;
        do {
            steps++;
            n--;
        } while (n > 0);
        return steps;
    }

    public static int firstDiagonal(int[][] grid) {
        outer:
        for (int row = 0; row < grid.length; row++) {
            for (int column = 0; column < grid[row].length; column++) {
                if (column > row) {
                    continue outer;
                }
                if (column == row && (grid[row][column] > 0 || grid[row][column] == -1)) {
                    return grid[row][column];
                }
            }
        }
        return 0;
    }

    public static Integer parse(String text) {
        try {
            return Integer.parseInt(text.trim());
        } catch (NumberFormatException error) {
            return null;
        } finally {
            calls++;
        }
    }

    public static int calls = 0;

    public static String firstLine(String text) throws IOException {
        try (BufferedReader reader = new BufferedReader(new StringReader(text))) {
            String line = reader.readLine();
            return line != null ? line : "empty";
        }
    }

    public static List<String> loud(List<String> words) {
        return words.stream()
            .filter(word -> word.length() > 2 && !word.startsWith("x"))
            .map(String::toUpperCase)
            .collect(Collectors.toList());
    }
}
`,
  'src/test/java/zoo/FlowTest.java': `package zoo;

import static org.junit.jupiter.api.Assertions.*;

import java.util.List;
import org.junit.jupiter.api.Test;

class FlowTest {
    @Test
    void switches() {
        assertEquals("A", Flow.grade(95));
        assertEquals("A", Flow.grade(100));
        assertEquals("B+", Flow.grade(87));
        assertEquals("B", Flow.grade(81));
        assertEquals("F", Flow.grade(10));
    }

    @Test
    void patterns() {
        assertEquals("text a", Flow.describe("a"));
        assertEquals("other", Flow.describe(""));
        assertEquals("axis", Flow.describe(new Flow.Point(0, 3)));
        assertEquals("axis", Flow.describe(new Flow.Point(3, 0)));
        assertEquals("point", Flow.describe(new Flow.Point(1, 1)));
        assertEquals("other", Flow.describe(4));
    }

    @Test
    void loops() {
        assertEquals(3, Flow.sumUntil(List.of(1, 2, 9, 4), 9));
        assertEquals(3, Flow.sumUntil(List.of(1, 2), 9));
        assertEquals(0, Flow.sumUntil(List.of(), 9));
        assertEquals(3, Flow.steps(3));
        assertEquals(1, Flow.steps(0));
        assertEquals(4, Flow.firstDiagonal(new int[][] {{0, 9}, {9, 4}}));
        assertEquals(-1, Flow.firstDiagonal(new int[][] {{-1}}));
        assertEquals(0, Flow.firstDiagonal(new int[][] {{0}}));
        assertEquals(0, Flow.firstDiagonal(new int[][] {}));
        assertEquals(0, Flow.firstDiagonal(new int[][] {{}}));
    }

    @Test
    void exceptionsAndResources() throws Exception {
        assertEquals(3, Flow.parse(" 3 "));
        assertNull(Flow.parse("x"));
        assertThrows(NullPointerException.class, () -> Flow.parse(null));
        assertEquals(3, Flow.calls);
        assertEquals("a", Flow.firstLine("a\\nb"));
        assertEquals("empty", Flow.firstLine(""));
    }

    @Test
    void streams() {
        assertEquals(List.of("ABC"), Flow.loud(List.of("abc", "ab", "xyz")));
    }
}
`,
};

const environment = { ...process.env, NO_COLOR: '1', CI: '1', ...(javaHome ? { JAVA_HOME: javaHome, PATH: `${javaHome}/bin:${process.env.PATH}` } : {}) };
const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: environment });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  // The plain run may download what Maven lacks; the measured one then has it.
  const plain = run('mvn', ['-q', 'test']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', 'mvn', '-q', '-o', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  const entry = byFile['src/main/java/zoo/Flow.java'];
  assert.ok(entry, `Flow.java was not measured\n${report}`);
  for (const metric of ['lines', 'branches', 'mcdcConditions']) {
    assert.equal(entry.covered[metric], entry.totals[metric], `Flow.java: ${metric}\n${report}`);
  }
  assert.ok(entry.totals.mcdcConditions > 0, report);
  // A narrowing `instanceof` cannot carry operand probes: the two say so.
  assert.equal(entry.measurementLimitations, 2, report);
  console.log('[java-syntax-zoo] every construct the JVM instrumenter rewrites reads as its tests drove it');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
