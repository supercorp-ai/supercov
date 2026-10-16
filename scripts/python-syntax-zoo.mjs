// Every construct the Python probes measure, end to end.
//
//   node scripts/python-syntax-zoo.mjs
//
// A small package whose unittest suite drives each construct both ways:
// comprehensions with conditions, assignment expressions, `match` with guards,
// chained comparisons, `and`/`or` as values (decisions where compound),
// try/except/else/finally, for/else and while/else, generators, coroutines,
// lambdas, decorators,
// f-strings, `assert`, `global` and `nonlocal`. Each file is read back for
// what its tests showed: a construct measured wrongly reads as uncovered.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-python-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

// A CPython with `match`: the one named, or the newest on PATH.
function python() {
  const candidates = process.env.SUPERCOV_PYTHON
    ? [process.env.SUPERCOV_PYTHON]
    : ['python3.14', 'python3.13', 'python3.12', 'python3.11', 'python3.10', 'python3'];
  for (const candidate of candidates) {
    const asked = spawnSync(candidate, ['-c', 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)']);
    if (asked.status === 0) return candidate;
  }
  throw new Error('python-syntax-zoo needs CPython 3.10 or newer (or SUPERCOV_PYTHON)');
}

const files = {
  'zoo/__init__.py': '',
  'zoo/flow.py': `import asyncio

COUNT = 0


def evens_over(values, limit):
    return [v for v in values if v % 2 == 0 and v > limit]


def first_long(words):
    if (n := len(words)) > 2:
        return n
    return 0


def classify(point):
    match point:
        case (0, 0):
            return "origin"
        case (x, 0) if x > 0:
            return "positive axis"
        case (x, y) if x == y:
            return "diagonal"
        case _:
            return "elsewhere"


def between(low, value, high):
    return low < value < high


def name_or_default(name, fallback):
    return name or fallback


def both(a, b):
    return a and b


def first_set(a, b, default):
    return a or b or default


def grouped_value(a, b, c):
    return (a and b) or c


def negated(a, b):
    return not (a and b)


def parse(text):
    try:
        value = int(text)
    except ValueError:
        return None
    else:
        return value * 2
    finally:
        global COUNT
        COUNT += 1


def find(items, target):
    for item in items:
        if item == target:
            break
    else:
        return -1
    return items.index(target)


def countdown(n):
    steps = 0
    while n > 0:
        n -= 1
        steps += 1
    else:
        steps += 100
    return steps


def squares(limit):
    for n in range(limit):
        if n % 2:
            yield n * n


async def fetch(value):
    await asyncio.sleep(0)
    return value if value else "empty"


def fetched(value):
    return asyncio.run(fetch(value))


def shout(function):
    def wrapper(*args):
        result = function(*args)
        return result.upper() if isinstance(result, str) else result
    return wrapper


@shout
def greet(name):
    return f"hello {name}" if name else "hello"


@shout
def count(items):
    return len(items)


def counter():
    total = 0

    def add(step):
        nonlocal total
        total += step if step > 0 else 0
        return total

    return add


def checked(value):
    assert value is not None and value >= 0, "value must be present and not negative"
    return value


sign = lambda n: "negative" if n < 0 else "not negative"
`,
  'tests/__init__.py': '',
  'tests/test_flow.py': `import unittest

from zoo import flow


class FlowTest(unittest.TestCase):
    def test_comprehension_and_walrus(self):
        self.assertEqual(flow.evens_over([1, 2, 4, 6], 3), [4, 6])
        self.assertEqual(flow.evens_over([], 3), [])
        self.assertEqual(flow.first_long(["a", "b", "c"]), 3)
        self.assertEqual(flow.first_long(["a"]), 0)

    def test_match(self):
        self.assertEqual(flow.classify((0, 0)), "origin")
        self.assertEqual(flow.classify((2, 0)), "positive axis")
        self.assertEqual(flow.classify((-2, 0)), "elsewhere")
        self.assertEqual(flow.classify((3, 3)), "diagonal")
        self.assertEqual(flow.classify((1, 2)), "elsewhere")

    def test_values(self):
        self.assertTrue(flow.between(1, 2, 3))
        self.assertFalse(flow.between(1, 5, 3))
        self.assertFalse(flow.between(4, 2, 3))
        self.assertEqual(flow.name_or_default("ann", "x"), "ann")
        self.assertEqual(flow.name_or_default("", "x"), "x")
        self.assertEqual(flow.both(1, 2), 2)
        self.assertEqual(flow.both(0, 2), 0)
        # Used as values, they return their operands, not their truth.
        self.assertEqual(flow.first_set("x", 0, "d"), "x")
        self.assertEqual(flow.first_set(0, "y", "d"), "y")
        self.assertEqual(flow.first_set(0, 0, "d"), "d")
        self.assertEqual(flow.grouped_value([1], [2], "c"), [2])
        self.assertEqual(flow.grouped_value([], [2], "c"), "c")
        self.assertEqual(flow.grouped_value([1], [], "c"), "c")
        self.assertIs(flow.negated(1, 1), False)
        self.assertIs(flow.negated(0, 1), True)
        self.assertIs(flow.negated(1, 0), True)
        self.assertEqual(flow.sign(-1), "negative")
        self.assertEqual(flow.sign(1), "not negative")

    def test_exceptions_and_loops(self):
        self.assertEqual(flow.parse("2"), 4)
        self.assertIsNone(flow.parse("x"))
        with self.assertRaises(TypeError):
            flow.parse(None)
        self.assertEqual(flow.COUNT, 3)
        self.assertEqual(flow.find([1, 2], 2), 1)
        self.assertEqual(flow.find([1], 2), -1)
        self.assertEqual(flow.find([], 2), -1)
        self.assertEqual(flow.countdown(2), 102)
        self.assertEqual(flow.countdown(0), 100)
        self.assertEqual(list(flow.squares(4)), [1, 9])
        self.assertEqual(list(flow.squares(0)), [])

    def test_coroutines_decorators_closures(self):
        self.assertEqual(flow.fetched(3), 3)
        self.assertEqual(flow.fetched(0), "empty")
        self.assertEqual(flow.greet("ann"), "HELLO ANN")
        self.assertEqual(flow.greet(""), "HELLO")
        self.assertEqual(flow.count([1, 2]), 2)
        add = flow.counter()
        self.assertEqual(add(2), 2)
        self.assertEqual(add(-1), 2)
        self.assertEqual(flow.checked(1), 1)
        with self.assertRaises(AssertionError):
            flow.checked(None)
        with self.assertRaises(AssertionError):
            flow.checked(-1)


if __name__ == "__main__":
    unittest.main()
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  const interpreter = python();
  const command = [interpreter, '-m', 'unittest', '-q'];
  const plain = run(command[0], command.slice(1));
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', ...command]);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  const entry = byFile['zoo/flow.py'];
  assert.ok(entry, `zoo/flow.py was not measured\n${report}`);
  for (const metric of ['lines', 'branches', 'mcdcConditions']) {
    assert.equal(entry.covered[metric], entry.totals[metric], `zoo/flow.py: ${metric}\n${report}`);
  }
  assert.ok(entry.totals.mcdcConditions > 0, report);
  assert.equal(entry.measurementLimitations, 0, report);
  console.log('[python-syntax-zoo] every construct the Python probes measure reads as its tests drove it');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
