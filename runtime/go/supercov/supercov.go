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
)

// What one test reached: the probes it set and the decision vectors it
// established, as sparse pairs rather than a copy of the whole array.
type testRecord struct {
	name    string
	probes  []probeHit
	vectors []vectorHit
}

type probeHit struct {
	index uint32
	value uint32
}

type vectorHit struct {
	decision uint32
	key      uint64
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
	// The keys most recently recorded, so a decision evaluated in a loop can
	// answer "seen this already" without a call. Two entries because the
	// common shape is a condition alternating between outcomes; a third
	// distinct vector simply pays for the call.
	recent     [2]uint64
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

var (
	// The array instrumented packages store into. Its contents are reset at
	// every test boundary; the header never changes after Reserve, so a probe
	// running on a leaked goroutine cannot see a torn slice.
	hits    []uint32
	global  []uint32
	records []testRecord
	current int
	widths  []uint8
	size    int
	mu      sync.Mutex
)

// Arm records how many conditions each decision has. The probe array is
// already in place by then: packages reserve it as they initialise.
func Arm(count int, conditions []uint8) {
	mu.Lock()
	defer mu.Unlock()
	if hits == nil {
		hits = make([]uint32, count)
		global = make([]uint32, count)
		size = count
	}
	widths = conditions
	states = make([]decisionState, len(conditions))
	for index, width := range conditions {
		states[index].width = width
	}
	suspended = suspended[:0]
	current = -1
	for id := range overflow {
		delete(overflow, id)
	}
}

// EnterTest binds every probe that fires next to this test, and returns the
// function that unbinds it.
//
// Attribution is a harvest at the boundary rather than a lookup on every
// probe: whatever the array holds now belongs to whoever was running, so it is
// swept into their record and cleared. That is one pass over the probes per
// test — a few microseconds — in exchange for removing an indirection from the
// hottest path a coverage tool has.
func EnterTest(name string) func() {
	mu.Lock()
	harvest()
	records = append(records, testRecord{name: name})
	current = len(records) - 1
	mu.Unlock()
	return func() {
		mu.Lock()
		harvest()
		current = -1
		mu.Unlock()
	}
}

// harvest moves everything the probe array holds into the running test's
// record and clears it. Execution outside any test still reaches the run-wide
// totals; it simply belongs to no test.
func harvest() {
	var into *testRecord
	if current >= 0 && current < len(records) {
		into = &records[current]
	}
	for index, value := range hits {
		if value == 0 {
			continue
		}
		global[index] |= value
		if into != nil {
			into.probes = append(into.probes, probeHit{uint32(index), value})
		}
		hits[index] = 0
	}
	for id := range states {
		state := &states[id]
		if state.count > 0 && into != nil {
			for _, key := range state.slots[:state.count] {
				into.vectors = append(into.vectors, vectorHit{uint32(id), key})
			}
		}
		state.count = 0
	}
	for id, keys := range overflow {
		if into != nil {
			for key := range keys {
				into.vectors = append(into.vectors, vectorHit{id, key})
			}
		}
		delete(overflow, id)
	}
}

// Reserve hands every instrumented package the one array their probes store
// into, allocating it on the first call.
//
// This is the shape Go's own cover tool and JaCoCo both settled on, and for
// the same reason: a probe should be a single store into an array the code
// already holds, not a call that has to find the array first. `hits[5] = 2`
// compiles to one move; reaching the same word through a function means a
// global load and a bounds check before anything is written.
//
// Package-level variables initialise in dependency order, so this package's
// own init has run before any generated `var _ = supercov.Reserve(...)`.
func Reserve(count int) []uint32 {
	mu.Lock()
	defer mu.Unlock()
	if hits == nil {
		hits = make([]uint32, count)
		global = make([]uint32, count)
		size = count
	}
	return hits
}

// P records that a point was reached, for callers that hold no array — the
// generated harness rather than generated probes.
func P(id uint32) {
	if int(id) < len(hits) {
		hits[id] = 2
	}
}

// A records that a branch alternative was taken.
func A(id uint32) { P(id) }

// C observes one condition of a decision and returns its value unchanged, so
// wrapping an operand cannot change what the expression evaluates to. Go
// evaluates a call argument only when the call is reached, which is what keeps
// `&&` and `||` short-circuiting through the wrapper.
func C(id uint32, index uint8, value bool) bool {
	if uint(id) >= uint(len(states)) || index >= 64 {
		return value
	}
	state := &states[id]
	bit := uint64(1) << index
	if index == 0 {
		// B clears the mask when it closes an evaluation, so a non-zero mask
		// here means this decision is already open: recursion arrived through
		// one of its own operands.
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

// B records which arm of a branch a condition selects and returns the value
// unchanged.
//
// The form for a branch whose condition is a single operand, which is most of
// them: no independence obligation, no call, and small enough that the
// compiler inlines it into the caller.
func B(whenTrue, whenFalse uint32, value bool) bool {
	if value {
		if int(whenTrue) < len(hits) {
			hits[whenTrue] = 2
		}
	} else if int(whenFalse) < len(hits) {
		hits[whenFalse] = 2
	}
	return value
}

// BD is B for a branch whose condition has more than one operand: it also
// closes the decision those operands opened.
//
// One call rather than two. Every decision Supercov records sits in an `if` or
// a `for`, so its outcome is the value this wrapper already holds — a separate
// outcome wrapper would observe it a second time, and profiling put 38% of an
// instrumented tight loop in exactly that second call.
func BD(whenTrue, whenFalse, id uint32, value bool) bool {
	if value {
		if int(whenTrue) < len(hits) {
			hits[whenTrue] = 2
		}
	} else if int(whenFalse) < len(hits) {
		hits[whenFalse] = 2
	}
	if uint(id) >= uint(len(states)) {
		return value
	}
	state := &states[id]
	key := state.evaluating | state.truth<<packedValueShift
	if value {
		key |= 1 << packedOutcomeShift
	}
	state.evaluating = 0
	// Nothing is suspended in the ordinary case, so no outer evaluation is
	// waiting to be restored and a cached key can return at once.
	if len(suspended) == 0 && (key == state.recent[0] || key == state.recent[1]) {
		return value
	}
	remember(state, id, key)
	return value
}

// remember holds everything B does not need on the path most evaluations
// take: restoring an evaluation that recursion interrupted, and recording a
// vector this test has not produced before.
//
//go:noinline
func remember(state *decisionState, id uint32, key uint64) {
	if len(suspended) > 0 {
		if last := suspended[len(suspended)-1]; last.id == id {
			state.evaluating, state.truth = last.evaluating, last.truth
			suspended = suspended[:len(suspended)-1]
		}
	}
	mask := key & ((1 << packedValueShift) - 1)
	if mask == 0 || state.width > packedMaxWidth {
		return
	}
	state.recent[1] = state.recent[0]
	state.recent[0] = key
	for offset := uint8(0); offset < state.count; offset++ {
		if state.slots[offset] == key {
			return
		}
	}
	if state.count < inlineSlots {
		state.slots[state.count] = key
		state.count++
		return
	}
	rare(id, key)
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

// Write emits the evidence transport the engine reads. The run-wide totals
// accumulate as tests are harvested, so they cannot disagree with the per-test
// records they come from.
func Write(path string) error {
	mu.Lock()
	defer mu.Unlock()
	harvest()
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
	if err := put(uint64(len(records))); err != nil {
		return err
	}
	for _, record := range records {
		if err := put(uint64(len(record.name))); err != nil {
			return err
		}
		if _, err := out.WriteString(record.name); err != nil {
			return err
		}
		// Sparse: only what this test reached, so the transport is
		// proportional to what ran rather than to the size of the project.
		if err := put(uint64(len(record.probes))); err != nil {
			return err
		}
		for _, hit := range record.probes {
			if err := put(uint64(hit.index)); err != nil {
				return err
			}
			if err := put(uint64(hit.value)); err != nil {
				return err
			}
		}
		// The vectors this test established, so independence can be traced to
		// the test that proved it rather than to the run as a whole.
		if err := put(uint64(len(record.vectors))); err != nil {
			return err
		}
		for _, hit := range record.vectors {
			if err := put(uint64(hit.decision)); err != nil {
				return err
			}
			if err := put(hit.key); err != nil {
				return err
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
		union := []uint64{}
		for _, record := range records {
			for _, hit := range record.vectors {
				if int(hit.decision) != index {
					continue
				}
				found := false
				for _, existing := range union {
					if existing == hit.key {
						found = true
						break
					}
				}
				if !found {
					union = append(union, hit.key)
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
