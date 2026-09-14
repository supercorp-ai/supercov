// Package supercov is the runtime half of Supercov's Go frontend.
//
// Instrumented source calls into it on every obligation, so a probe costs
// exactly what this package's hot path costs. For a point that is one atomic
// pointer load, one bounds check and one store — small enough that the
// compiler inlines it into the caller. Anything that cannot be done in a store
// happens at a test boundary instead, where it is paid once per test rather
// than once per statement.
package supercov

import (
	"bufio"
	"encoding/binary"
	"os"
	"sync"
	"sync/atomic"
)

// A bucket is one slice of per-probe bitmasks: bit 0 for false, bit 1 for
// true. A point or an alternative only ever sets bit 1.
//
// There is always exactly one active bucket, so the hot path never asks
// whether a test is running. Execution outside any test — package
// initialisers, and anything a leaked goroutine does after its test finished —
// lands in bucket zero rather than being charged to whichever test was current.
type bucket struct {
	name   string
	probes []uint32
	// Distinct decision vectors this test produced, one slice per decision.
	// They live here rather than globally because MC/DC evidence is only
	// useful if it says which test established independence.
	vectors [][]uint64
}

// One decision's entire state, in one struct.
//
// C and D touch all of it on every evaluation, so splitting it across parallel
// arrays costs a separate global slice header load per field. Held together,
// `&states[id]` is one header load and one bounds check, and everything after
// is an offset from that pointer.
//
// Short-circuiting means a condition may not be evaluated at all, and MC/DC
// asks a different question of an unevaluated operand than of a false one.
// Keeping `evaluating` separate from `truth` preserves that; collapsing them
// would report `a && b` with `a` false as though `b` had been tested.
type decisionState struct {
	evaluating uint64
	truth      uint64
	width      uint8
	count      uint8
	// Distinct vectors seen in the current test. Inline rather than a slice:
	// a decision of n conditions has at most 2^(n+1) of them, so three
	// conditions fit exactly and wider ones spill to a map nothing reaches in
	// practice.
	slots [16]uint64
}

const inlineSlots = 16

var (
	states []decisionState
	// Evaluations interrupted by recursion through one of their own operands.
	// `if valid(n-1) && x` can re-enter the same decision, and without this the
	// inner evaluation would consume the outer one's partial vector.
	suspended []suspension
	// Vectors from decisions too wide for their inline room. A map because
	// nothing reaches it in practice, and one never touched costs nothing.
	overflow = map[uint32]map[uint64]bool{}
)

type suspension struct {
	id         uint32
	evaluating uint64
	truth      uint64
}

const (
	packedValueShift   = 24
	packedOutcomeShift = 48
	// Above this a vector no longer fits one word. No real decision has 24
	// independent conditions; one that did would be unreadable long before it
	// was untestable.
	packedMaxWidth = 24
)

var empty = &bucket{}

var (
	active  atomic.Pointer[bucket]
	buckets []*bucket
	widths  []uint8
	size    int
	mu      sync.Mutex
)

func init() { active.Store(empty) }

// Arm prepares the runtime for a run of `count` probes, where `conditions`
// gives the number of conditions in each decision. The generated harness calls
// it before any test runs.
func Arm(count int, conditions []uint8) {
	mu.Lock()
	defer mu.Unlock()
	size = count
	widths = conditions
	states = make([]decisionState, len(conditions))
	for index, width := range conditions {
		states[index].width = width
	}
	suspended = suspended[:0]
	for id := range overflow {
		delete(overflow, id)
	}
	outside := newBucket("")
	buckets = []*bucket{outside}
	active.Store(outside)
}

func newBucket(name string) *bucket {
	return &bucket{
		name:    name,
		probes:  make([]uint32, size),
		vectors: make([][]uint64, len(widths)),
	}
}

// EnterTest binds every probe that fires next to this test, and returns the
// function that unbinds it.
//
// Go runs a package's test functions sequentially unless one opts into
// parallelism, and Supercov runs one package at a time, so a single active
// bucket is exact rather than approximate.
func EnterTest(name string) func() {
	previous := active.Load()
	if previous == empty {
		return func() {}
	}
	mu.Lock()
	flushVectors(previous)
	next := newBucket(name)
	buckets = append(buckets, next)
	mu.Unlock()
	active.Store(next)
	return func() {
		mu.Lock()
		flushVectors(next)
		mu.Unlock()
		active.Store(previous)
	}
}

// flushVectors hands the vectors recorded since the last change to the test
// that produced them, and clears the counters for the next one.
func flushVectors(into *bucket) {
	if into == nil || into == empty {
		return
	}
	for id := range states {
		state := &states[id]
		if state.count > 0 && id < len(into.vectors) {
			into.vectors[id] = append(into.vectors[id], state.slots[:state.count]...)
		}
		state.count = 0
	}
	for id, keys := range overflow {
		if int(id) < len(into.vectors) {
			for key := range keys {
				into.vectors[id] = append(into.vectors[id], key)
			}
		}
		delete(overflow, id)
	}
}

// P records that a point — a statement or a function — was reached.
func P(id uint32) {
	b := active.Load()
	if int(id) < len(b.probes) {
		// A plain store, not `|=`. A point only ever records "reached", so
		// there is no earlier bit to preserve and no reason to read the word
		// back before writing it.
		b.probes[id] = 2
	}
}

// A records that a branch alternative was taken.
func A(id uint32) { P(id) }

// B records which arm of a branch a condition selects, and returns the value
// unchanged. The arm that was not taken stays unset, which is what makes an
// untested guard visible rather than invisible.
func B(whenTrue, whenFalse uint32, value bool) bool {
	if value {
		P(whenTrue)
	} else {
		P(whenFalse)
	}
	return value
}

// C observes one condition of a decision and returns its value unchanged, so
// wrapping an operand cannot change what the expression evaluates to. Go
// evaluates a call argument only when the call is reached, which is what keeps
// `&&` and `||` short-circuiting through the wrapper.
func C(id uint32, index uint8, value bool) bool {
	if int(id) >= len(states) || index >= 64 {
		return value
	}
	state := &states[id]
	bit := uint64(1) << index
	if index == 0 {
		// D clears both words when it consumes an evaluation, so a non-zero
		// mask here means this decision is already open: recursion arrived
		// through one of its own operands.
		if state.evaluating != 0 {
			suspended = append(suspended, suspension{id, state.evaluating, state.truth})
		}
		state.evaluating, state.truth = bit, 0
	} else {
		state.evaluating |= bit
	}
	if value {
		state.truth |= bit
	}
	return value
}

// D observes a decision's outcome and returns it unchanged, closing the
// evaluation its conditions opened and recording the vector against the test
// that produced it.
//
// Neither C nor D records a point. The vector already says exactly which
// conditions were evaluated and what the decision came to, so a probe beside
// it would be the same fact stored twice, paid for on the hottest path
// instrumentation has.
func D(id uint32, value bool) bool {
	if int(id) >= len(states) {
		return value
	}
	state := &states[id]
	mask, values := state.evaluating, state.truth
	state.evaluating, state.truth = 0, 0
	if len(suspended) > 0 {
		if last := suspended[len(suspended)-1]; last.id == id {
			state.evaluating, state.truth = last.evaluating, last.truth
			suspended = suspended[:len(suspended)-1]
		}
	}
	if mask == 0 || state.width > packedMaxWidth {
		return value
	}
	key := mask | values<<packedValueShift
	if value {
		key |= 1 << packedOutcomeShift
	}
	// A handful of register compares over words that share a cache line with
	// the state just read, which is why this beats hashing per evaluation.
	for offset := uint8(0); offset < state.count; offset++ {
		if state.slots[offset] == key {
			return value
		}
	}
	if state.count < inlineSlots {
		state.slots[state.count] = key
		state.count++
		return value
	}
	rare(id, key)
	return value
}

func rare(id uint32, key uint64) {
	keys := overflow[id]
	if keys == nil {
		keys = map[uint64]bool{}
		overflow[id] = keys
	}
	keys[key] = true
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
	flushVectors(active.Load())
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
		// The vectors this test established, so independence can be traced to
		// the test that proved it rather than to the run as a whole.
		vectorCount := 0
		for _, keys := range b.vectors {
			vectorCount += len(keys)
		}
		if err := put(uint64(vectorCount)); err != nil {
			return err
		}
		for index, keys := range b.vectors {
			for _, key := range keys {
				if err := put(uint64(index)); err != nil {
					return err
				}
				if err := put(key); err != nil {
					return err
				}
			}
		}
	}
	if err := put(uint64(len(widths))); err != nil {
		return err
	}
	for index, width := range widths {
		if err := put(uint64(width)); err != nil {
			return err
		}
		// The run-wide set is the union of what the tests saw, so it cannot
		// disagree with the per-test records it comes from.
		union := []uint64{}
		for _, b := range buckets {
			if index >= len(b.vectors) {
				continue
			}
			for _, key := range b.vectors[index] {
				found := false
				for _, existing := range union {
					if existing == key {
						found = true
						break
					}
				}
				if !found {
					union = append(union, key)
				}
			}
		}
		if err := put(uint64(len(union))); err != nil {
			return err
		}
		for _, key := range union {
			if err := put(key); err != nil {
				return err
			}
		}
	}
	return out.Flush()
}
