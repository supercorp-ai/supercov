// Package supercov is the runtime half of Supercov's Go frontend.
//
// Instrumented source calls into it on every obligation, so the cost of a
// probe is the cost of this package's hot path and nothing else. That path is
// a single store into a preallocated slice: no locks, no allocation, no map
// lookup, no interface dispatch. Anything that cannot be done in a store is
// done at a test boundary instead, where it is paid once per test rather than
// once per statement.
package supercov

import (
	"bufio"
	"encoding/binary"
	"os"
	"sync"
	"sync/atomic"
)

// Probe kinds, matching the manifest the engine wrote.
const (
	kindPoint       = 0
	kindAlternative = 1
	kindCondition   = 2
	kindOutcome     = 3
)

// hits is indexed by probe id. Each entry is a bitmask of the values that
// probe was observed with: bit 0 for false, bit 1 for true. A point or an
// alternative only ever records bit 1.
//
// Preallocated once, so a probe never grows a slice and never allocates.
var (
	hits    []uint32
	current atomic.Int32
	tests   []testRecord
	mu      sync.Mutex
	armed   atomic.Bool
)

type testRecord struct {
	name   string
	first  int
	probes []uint32
}

// Arm prepares the runtime for a run of `size` probes. The generated package
// initialiser calls it before any test runs.
func Arm(size int) {
	mu.Lock()
	defer mu.Unlock()
	hits = make([]uint32, size)
	current.Store(-1)
	armed.Store(true)
}

// EnterTest binds every probe that fires next to this test, and returns the
// function that unbinds it.
//
// Go runs the test functions of one package sequentially unless a test calls
// t.Parallel(), and Supercov runs one package at a time, so a single current
// index is exact rather than approximate. A test that opts into parallelism is
// reported as a limitation instead of being attributed by guesswork.
func EnterTest(name string) func() {
	if !armed.Load() {
		return func() {}
	}
	mu.Lock()
	index := len(tests)
	tests = append(tests, testRecord{name: name, probes: make([]uint32, len(hits))})
	mu.Unlock()
	previous := current.Swap(int32(index))
	return func() { current.Store(previous) }
}

// P records that a point -- a statement or a function -- was reached.
func P(id uint32) {
	record(id, 2)
}

// A records that a branch alternative was taken.
func A(id uint32) {
	record(id, 2)
}

// B records which arm of a branch a condition selects, and returns the value
// unchanged. One call rather than two probes keeps the hot path to a single
// record, and records the arm that was *not* taken by never setting its bit --
// which is what makes an untested guard visible instead of invisible.
func B(whenTrue, whenFalse uint32, value bool) bool {
	if value {
		record(whenTrue, 2)
	} else {
		record(whenFalse, 2)
	}
	return value
}

// C observes one condition's value and returns it unchanged, so wrapping an
// operand cannot change what the expression evaluates to. Go evaluates a call
// argument only when the call is reached, which is what keeps `&&` and `||`
// short-circuiting through the wrapper.
func C(id uint32, value bool) bool {
	record(id, mask(value))
	return value
}

// D observes a decision's outcome and returns it unchanged.
func D(id uint32, value bool) bool {
	record(id, mask(value))
	return value
}

func mask(value bool) uint32 {
	if value {
		return 2
	}
	return 1
}

// record is the hot path: two bounds-checked slice stores and nothing else.
func record(id uint32, bits uint32) {
	index := int(id)
	if index >= len(hits) {
		return
	}
	hits[index] |= bits
	if at := current.Load(); at >= 0 {
		test := &tests[at]
		if index < len(test.probes) {
			test.probes[index] |= bits
		}
	}
}

// Write emits the evidence transport the engine reads. The generated harness
// calls it once, after every test in the package has finished.
func Write(path string) error {
	mu.Lock()
	defer mu.Unlock()
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()
	out := bufio.NewWriterSize(file, 1<<20)
	var scratch [8]byte
	put := func(value uint64) error {
		binary.LittleEndian.PutUint64(scratch[:], value)
		_, err := out.Write(scratch[:])
		return err
	}
	if err := put(uint64(len(hits))); err != nil {
		return err
	}
	for _, value := range hits {
		if err := put(uint64(value)); err != nil {
			return err
		}
	}
	if err := put(uint64(len(tests))); err != nil {
		return err
	}
	for _, test := range tests {
		if err := put(uint64(len(test.name))); err != nil {
			return err
		}
		if _, err := out.WriteString(test.name); err != nil {
			return err
		}
		// Only probes this test actually reached, so the transport is
		// proportional to what ran rather than to the size of the project.
		count := 0
		for _, value := range test.probes {
			if value != 0 {
				count++
			}
		}
		if err := put(uint64(count)); err != nil {
			return err
		}
		for index, value := range test.probes {
			if value == 0 {
				continue
			}
			if err := put(uint64(index)); err != nil {
				return err
			}
			if err := put(uint64(value)); err != nil {
				return err
			}
		}
	}
	return out.Flush()
}
