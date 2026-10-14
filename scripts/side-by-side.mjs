// Runs of one project started at the same moment each have a copy of the
// project to themselves, and all of them are recorded.
//
//   node scripts/side-by-side.mjs python ruby go rust node jvm
//
// Each language gets one small project with a test that takes a moment, and
// three measured runs of it are started together, twice: once with nothing
// stored, when all three make what the store needs, and once after. Every run
// has to pass, be recorded, and read as the same run alone reads. Before
// this, the second and third were refused: every run rebuilt the one copy.
// A language whose toolchain is not installed is refused, not skipped.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const AT_ONCE = 3;

function project(files) {
  const root = resolve(mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-side-by-side-')), 'shop');
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  spawnSync('git', ['init', '-q'], { cwd: root });
  return root;
}

function measured(root, command) {
  return new Promise((done) => {
    const child = spawn(binary, ['--', ...command], { cwd: root, env: { ...process.env, NO_COLOR: '1', CI: '1' } });
    let printed = '';
    child.stdout.on('data', (chunk) => (printed += chunk));
    child.stderr.on('data', (chunk) => (printed += chunk));
    child.on('error', (error) => done({ status: -1, printed: String(error) }));
    child.on('close', (status) => done({ status, printed }));
  });
}

function supercov(root, args) {
  const result = spawnSync(binary, args, { cwd: root, encoding: 'utf8' });
  assert.equal(result.error, undefined, `supercov did not start: ${result.error}`);
  return result;
}

const summary = (root, id) => {
  const data = JSON.parse(supercov(root, ['runs', id, '--json']).stdout).data;
  return { tests: data.tests, lines: `${data.coverage.lines.covered}/${data.coverage.lines.total}`, stale: data.stale };
};

// A Ruby that Supercov measures: the one named, Homebrew's where the system's
// is older, or the one on PATH. The same choice the Ruby gate makes.
function ruby() {
  const candidates = process.env.SUPERCOV_RUBY ? [process.env.SUPERCOV_RUBY] : ['/opt/homebrew/opt/ruby/bin/ruby', 'ruby'];
  for (const candidate of candidates) {
    const asked = spawnSync(candidate, ['-e', 'print RUBY_VERSION'], { encoding: 'utf8' });
    const [major, minor] = (asked.stdout ?? '').split('.').map(Number);
    if (asked.status === 0 && (major > 3 || (major === 3 && minor >= 3))) return candidate;
  }
  throw new Error('side-by-side needs Ruby 3.3 or newer on PATH (or SUPERCOV_RUBY)');
}

const PROJECTS = {
  node: {
    command: ['node', '--test'],
    files: {
      'package.json': '{ "name": "shop", "private": true, "type": "module" }\n',
      'src/price.js': 'export function half(price) {\n  if (price < 0) {\n    return 0;\n  }\n  return price / 2;\n}\n',
      'test/price.test.js': "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { half } from '../src/price.js';\n\ntest('half', async () => {\n  await new Promise((done) => setTimeout(done, 500));\n  assert.equal(half(10), 5);\n});\n",
    },
  },
  python: {
    command: [process.platform === 'win32' ? 'python' : 'python3', '-m', 'unittest', 'discover', '-s', 'tests'],
    files: {
      'shop/__init__.py': 'def half(price):\n    if price < 0:\n        return 0\n    return price / 2\n',
      'tests/test_price.py': 'import time\nimport unittest\n\nfrom shop import half\n\n\nclass Price(unittest.TestCase):\n    def test_half(self):\n        time.sleep(0.5)\n        self.assertEqual(half(10), 5)\n',
    },
  },
  ruby: {
    get command() {
      return [ruby(), '-Ilib', 'test/price_test.rb'];
    },
    files: {
      'lib/price.rb': 'module Price\n  def self.half(price)\n    return 0 if price.negative?\n\n    price / 2\n  end\nend\n',
      'test/price_test.rb': "require 'minitest/autorun'\nrequire 'price'\n\nclass PriceTest < Minitest::Test\n  def test_half\n    sleep 0.5\n    assert_equal 5, Price.half(10)\n  end\nend\n",
    },
  },
  go: {
    command: ['go', 'test', './...'],
    files: {
      'go.mod': 'module example.com/shop\n\ngo 1.22\n',
      'price.go': 'package shop\n\nfunc Half(price int) int {\n\tif price < 0 {\n\t\treturn 0\n\t}\n\treturn price / 2\n}\n',
      'price_test.go': 'package shop\n\nimport (\n\t"testing"\n\t"time"\n)\n\nfunc TestHalf(t *testing.T) {\n\ttime.Sleep(500 * time.Millisecond)\n\tif Half(10) != 5 {\n\t\tt.Fatal("half")\n\t}\n}\n',
    },
  },
  rust: {
    command: ['cargo', 'test'],
    files: {
      'Cargo.toml': '[package]\nname = "shop"\nversion = "0.1.0"\nedition = "2021"\n',
      'src/lib.rs': 'pub fn half(price: i32) -> i32 {\n    if price < 0 {\n        return 0;\n    }\n    price / 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn halves() {\n        std::thread::sleep(std::time::Duration::from_millis(500));\n        assert_eq!(super::half(10), 5);\n    }\n}\n',
    },
  },
  jvm: {
    command: ['mvn', '-q', 'test'],
    files: {
      'pom.xml': '<project xmlns="http://maven.apache.org/POM/4.0.0">\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>example</groupId>\n  <artifactId>shop</artifactId>\n  <version>1.0</version>\n  <properties>\n    <maven.compiler.source>17</maven.compiler.source>\n    <maven.compiler.target>17</maven.compiler.target>\n    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n  </properties>\n  <dependencies>\n    <dependency>\n      <groupId>org.junit.jupiter</groupId>\n      <artifactId>junit-jupiter</artifactId>\n      <version>5.10.2</version>\n      <scope>test</scope>\n    </dependency>\n  </dependencies>\n  <build>\n    <plugins>\n      <plugin>\n        <groupId>org.apache.maven.plugins</groupId>\n        <artifactId>maven-surefire-plugin</artifactId>\n        <version>3.2.5</version>\n      </plugin>\n    </plugins>\n  </build>\n</project>\n',
      'src/main/java/shop/Price.java': 'package shop;\n\npublic class Price {\n    public static int half(int price) {\n        if (price < 0) {\n            return 0;\n        }\n        return price / 2;\n    }\n}\n',
      'src/test/java/shop/PriceTest.java': 'package shop;\n\nimport static org.junit.jupiter.api.Assertions.assertEquals;\n\nimport org.junit.jupiter.api.Test;\n\nclass PriceTest {\n    @Test\n    void half() throws InterruptedException {\n        Thread.sleep(500);\n        assertEquals(5, Price.half(10));\n    }\n}\n',
    },
  },
};

const asked = process.argv.slice(2);
assert.ok(asked.length > 0 && asked.every((language) => language in PROJECTS), `usage: side-by-side.mjs ${Object.keys(PROJECTS).join(' ')}`);

for (const language of asked) {
  const { command, files } = PROJECTS[language];
  const root = project(files);
  // The run alone: what each of the runs at once has to read as.
  const alone = await measured(root, command);
  assert.equal(alone.status, 0, `${language}: the suite does not pass under Supercov alone:\n${alone.printed}`);
  const first = JSON.parse(supercov(root, ['runs', '--json']).stdout).data.runs[0].id;
  const expected = summary(root, first);
  assert.ok(expected.tests >= 1, `${language}: the run alone recorded no test`);

  for (const stored of ['nothing stored', 'a store already there']) {
    if (stored === 'nothing stored') assert.equal(supercov(root, ['runs', 'clean']).status, 0);
    const before = new Set(JSON.parse(supercov(root, ['runs', '--json']).stdout).data.runs.map((run) => run.id));
    const runs = await Promise.all(Array.from({ length: AT_ONCE }, () => measured(root, command)));
    for (const run of runs) {
      assert.equal(run.status, 0, `${language}, ${stored}: one of ${AT_ONCE} runs at once failed:\n${run.printed}`);
      assert.doesNotMatch(run.printed, /already active|is being cleaned/u);
    }
    const made = JSON.parse(supercov(root, ['runs', '--json']).stdout).data.runs.map((run) => run.id).filter((id) => !before.has(id));
    assert.equal(made.length, AT_ONCE, `${language}, ${stored}: ${made.length} of ${AT_ONCE} runs were recorded`);
    for (const id of made) assert.deepEqual(summary(root, id), expected, `${language}, ${stored}: run ${id} does not read as the run alone`);
  }

  // Cleaning is refused under a run, with status 2, and removes everything after it.
  const running = measured(root, command);
  await new Promise((done) => setTimeout(done, 250));
  let refused = supercov(root, ['runs', 'clean']);
  // A run that had not yet taken its share of the store when cleaning looked is refused itself.
  if (refused.status === 0) {
    const run = await running;
    assert.match(run.printed, /storage is being cleaned/u, `${language}: cleaning and a run both went ahead:\n${run.printed}`);
  } else {
    assert.equal(refused.status, 2, refused.stderr);
    assert.match(refused.stderr, /is active in this project/u);
    assert.equal((await running).status, 0);
    refused = supercov(root, ['runs', 'clean']);
    assert.equal(refused.status, 0, refused.stderr);
  }
  console.log(`[side-by-side] ${language}: ${AT_ONCE} runs at once each have a copy, pass, and read as the run alone`);
}
