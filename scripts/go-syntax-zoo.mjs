// Every construct the Go instrumenter rewrites, measured end to end.
//
//   node scripts/go-syntax-zoo.mjs
//
// A small module whose tests drive each construct both ways: `if` and
// `switch` with init statements, `fallthrough`, a type switch, `range` over an
// int, a labelled `continue`, `select`, `defer` with `recover`, closures,
// generics and compound conditions. Each file is read back for what its tests
// showed: a construct measured wrongly reads as uncovered here.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-go-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

const files = {
  'go.mod': 'module example.com/zoo\n\ngo 1.22\n',
  'flow/flow.go': `package flow

import (
	"errors"
	"strconv"
)

func Parse(text string) (int, error) {
	if n, err := strconv.Atoi(text); err == nil && n >= 0 {
		return n, nil
	}
	return 0, errors.New("not a count")
}

func Grade(score int) string {
	switch s := score / 10; {
	case s >= 9:
		return "A"
	case s == 8:
		fallthrough
	case s == 7:
		return "B or C"
	default:
		return "F"
	}
}

func Kind(value any) string {
	switch v := value.(type) {
	case int:
		if v < 0 {
			return "negative int"
		}
		return "int"
	case string:
		return "string"
	default:
		return "other"
	}
}

func Evens(n int) []int {
	out := []int{}
	for i := range n {
		if i%2 == 1 {
			continue
		}
		out = append(out, i)
	}
	return out
}

func FirstDiagonal(grid [][]int) int {
outer:
	for row, cells := range grid {
		for column, cell := range cells {
			if column > row {
				continue outer
			}
			if column == row && (cell > 0 || cell == -1) {
				return cell
			}
		}
	}
	return 0
}

func Receive(ready chan int, done chan struct{}) string {
	select {
	case n := <-ready:
		return strconv.Itoa(n)
	case <-done:
		return "done"
	}
}

func Safe(divisor int) (result int, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = errors.New("divided by zero")
		}
	}()
	return 10 / divisor, nil
}

func Counter() func(int) int {
	total := 0
	return func(step int) int {
		if step > 0 {
			total += step
		}
		return total
	}
}

func Max[T int | float64](a, b T) T {
	if a > b {
		return a
	}
	return b
}
`,
  'flow/flow_test.go': `package flow

import "testing"

func TestFlow(t *testing.T) {
	if n, err := Parse("3"); n != 3 || err != nil {
		t.Fatal(n, err)
	}
	if _, err := Parse("-1"); err == nil {
		t.Fatal("negative accepted")
	}
	if _, err := Parse("x"); err == nil {
		t.Fatal("x accepted")
	}
	for score, want := range map[int]string{95: "A", 85: "B or C", 75: "B or C", 10: "F"} {
		if got := Grade(score); got != want {
			t.Fatal(score, got)
		}
	}
	for value, want := range map[any]string{-1: "negative int", 1: "int", "s": "string", 1.5: "other"} {
		if got := Kind(value); got != want {
			t.Fatal(value, got)
		}
	}
	if got := Evens(5); len(got) != 3 {
		t.Fatal(got)
	}
	if got := Evens(0); len(got) != 0 {
		t.Fatal(got)
	}
	if got := FirstDiagonal([][]int{{0, 9}, {9, 4}}); got != 4 {
		t.Fatal(got)
	}
	if got := FirstDiagonal([][]int{{-1}}); got != -1 {
		t.Fatal(got)
	}
	if got := FirstDiagonal([][]int{{0}}); got != 0 {
		t.Fatal(got)
	}
	if got := FirstDiagonal(nil); got != 0 {
		t.Fatal(got)
	}
	if got := FirstDiagonal([][]int{{}}); got != 0 {
		t.Fatal(got)
	}
	ready := make(chan int, 1)
	ready <- 7
	if got := Receive(ready, nil); got != "7" {
		t.Fatal(got)
	}
	done := make(chan struct{})
	close(done)
	if got := Receive(nil, done); got != "done" {
		t.Fatal(got)
	}
	if got, err := Safe(2); got != 5 || err != nil {
		t.Fatal(got, err)
	}
	if _, err := Safe(0); err == nil {
		t.Fatal("no error")
	}
	add := Counter()
	if add(2) != 2 || add(-1) != 2 {
		t.Fatal("counter")
	}
	if Max(1, 2) != 2 || Max(2.5, 1.0) != 2.5 {
		t.Fatal("max")
	}
}
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  const plain = run('go', ['test', './...']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', 'go', 'test', './...']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  const entry = byFile['flow/flow.go'];
  assert.ok(entry, `flow/flow.go was not measured\n${report}`);
  for (const metric of ['lines', 'branches', 'mcdcConditions']) {
    assert.equal(entry.covered[metric], entry.totals[metric], `flow/flow.go: ${metric}\n${report}`);
  }
  assert.ok(entry.totals.mcdcConditions > 0, report);
  // Go range loops record no entered or zero-iteration outcome (the other
  // languages do): the three here say so, and nothing else is limited.
  assert.equal(entry.measurementLimitations, 3, report);
  console.log('[go-syntax-zoo] every construct the Go instrumenter rewrites reads as its tests drove it');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
