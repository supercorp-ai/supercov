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

// A bucket is one slice of per-probe bitmasks: bit 0 for false, bit 1 for
// true. A point or an alternative only ever sets bit 1.
//
// There is always exactly one active bucket, so the hot path never has to ask
// whether a test is running. Execution outside any test -- package
// initialisers, and anything a leaked goroutine does after its test finished --
// lands in bucket zero rather than being attributed to whichever test happened
// to be current.
type bucket struct {
	name   string
	probes []uint32
}

// Never nil, so the hot path needs no nil check: before Arm, the bucket is
// empty and every bounds check simply fails.
var empty = &bucket{}

var (
	active  atomic.Pointer[bucket]
	buckets []*bucket
	size    int
	mu      sync.Mutex
)

// Arm prepares the runtime for a run of `count` probes. The generated harness
// calls it before any test runs.
func init() { active.Store(empty) }

func Arm(count int) {
	mu.Lock()
	defer mu.Unlock()
	size = count
	outside := &bucket{name: "", probes: make([]uint32, count)}
	buckets = []*bucket{outside}
	active.Store(outside)
}

// EnterTest binds every probe that fires next to this test, and returns the
// function that unbinds it.
//
// Go runs a package's test functions sequentially unless one opts into
// parallelism, and Supercov runs one package at a time, so a single active
// bucket is exact rather than approximate. A test that calls t.Parallel() is
// reported as a limitation instead of being attributed by guesswork.
func EnterTest(name string) func() {
	previous := active.Load()
	if previous == empty {
		return func() {}
	}
	mu.Lock()
	next := &bucket{name: name, probes: make([]uint32, size)}
	buckets = append(buckets, next)
	mu.Unlock()
	active.Store(next)
	return func() { active.Store(previous) }
}

// P records that a point -- a statement or a function -- was reached.
//
// The whole hot path: one atomic pointer load, one bounds check, one store. No
// lock, no allocation, no map lookup, no interface dispatch, and small enough
// for the compiler to inline into the caller.
func P(id uint32) {
	b := active.Load()
	if int(id) < len(b.probes) {
		// A plain store, not `|=`. A point or an alternative only ever records
		// "reached", so there is no earlier bit to preserve and no reason to
		// read the word back before writing it.
		b.probes[id] = 2
	}
}

// A records that a branch alternative was taken.
func A(id uint32) {
	P(id)
}

// B records which arm of a branch a condition selects, and returns the value
// unchanged. One call rather than two probes keeps the hot path to a single
// record, and the arm that was not taken stays unset -- which is what makes an
// untested guard visible instead of invisible.
func B(whenTrue, whenFalse uint32, value bool) bool {
	if value {
		P(whenTrue)
	} else {
		P(whenFalse)
	}
	return value
}

// C observes one condition's value and returns it unchanged, so wrapping an
// operand cannot change what the expression evaluates to. Go evaluates a call
// argument only when the call is reached, which is what keeps `&&` and `||`
// short-circuiting through the wrapper.
func C(id uint32, value bool) bool {
	b := active.Load()
	if int(id) < len(b.probes) {
		// A condition genuinely accumulates: seeing it false must not forget
		// that it was once true, because MC/DC asks about both.
		b.probes[id] |= mask(value)
	}
	return value
}

// D observes a decision's outcome and returns it unchanged.
func D(id uint32, value bool) bool {
	return C(id, value)
}

func mask(value bool) uint32 {
	if value {
		return 2
	}
	return 1
}

// Finish writes the evidence and returns the exit code it was given.
//
// It wraps `m.Run()` rather than deferring, because the idiomatic TestMain
// ends in `os.Exit(m.Run())` and os.Exit runs no deferred function. Writing on
// the way through is the only placement that survives both shapes.
func Finish(code int, path string) int {
	if err := Write(path); err != nil {
		os.Stderr.WriteString("supercov: could not write coverage evidence: " + err.Error() + "\n")
	}
	return code
}

// Write emits the evidence transport the engine reads. The run-wide totals are
// the union of every bucket, so they cannot disagree with the per-test records
// they are derived from.
func Write(path string) error {
	mu.Lock()
	defer mu.Unlock()
	global := make([]uint32, size)
	for _, b := range buckets {
		for index, value := range b.probes {
			global[index] |= value
		}
	}
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
	if err := put(uint64(len(global))); err != nil {
		return err
	}
	for _, value := range global {
		if err := put(uint64(value)); err != nil {
			return err
		}
	}
	named := buckets[1:]
	if err := put(uint64(len(named))); err != nil {
		return err
	}
	for _, b := range named {
		if err := put(uint64(len(b.name))); err != nil {
			return err
		}
		if _, err := out.WriteString(b.name); err != nil {
			return err
		}
		// Only probes this test reached, so the transport is proportional to
		// what ran rather than to the size of the project.
		count := 0
		for _, value := range b.probes {
			if value != 0 {
				count++
			}
		}
		if err := put(uint64(count)); err != nil {
			return err
		}
		for index, value := range b.probes {
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
