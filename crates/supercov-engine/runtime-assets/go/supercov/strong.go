//go:build go1.21

package supercov

// Strong mutation's switches (docs/design/strong-mutation.md, Go).
//
// In strong mode every listed mutant of a file is written into it behind a
// switch, and one mutant is active per process: the one
// SUPERCOV_ACTIVE_MUTANT names (`<file>#<index>`, the file as mutants.json
// keys it, the index its place in that file's list). A test binary built
// once runs every mutant of its package, one process each.
//
// Each file reads its record once, as its package initialises: `var s =
// StrongSlot("a/b.go")`. A switch is then a comparison of the record's
// active index with the mutant's own, so a mutant that is not active costs
// one load and one compare where it stands.

import (
	"cmp"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
)

// Strong is one file's switch record: the index of its active mutant, or
// -1 when the active mutant is another file's or there is none.
type Strong struct {
	A int32
}

var (
	strongFile  string
	strongIndex int32 = -1
	// How many times the active mutant's switch may choose it before the
	// mutant counts as a timeout: a loop the mutant made endless ends in
	// the test binary rather than hanging it.
	strongHitLimit int64 = 1_000_000
	strongHits     atomic.Int64
	strongTimedOut sync.Once
)

// ActiveMutantEnv names the active mutant, `<file>#<index>`.
const ActiveMutantEnv = "SUPERCOV_ACTIVE_MUTANT"

// MutantHitLimitEnv overrides the hit limit.
const MutantHitLimitEnv = "SUPERCOV_MUTANT_HIT_LIMIT"

// StrongResultsEnv names the pass's results: a process whose mutant passes
// the hit limit appends a line to the file beside it (`.timeouts.jsonl`),
// so a timeout is told apart from a failure even where the code under test
// recovers the panic that reports it.
const StrongResultsEnv = "SUPERCOV_STRONG_RESULTS"

// MutantTimeout is what the switch panics with past the hit limit.
type MutantTimeout struct{}

func (MutantTimeout) Error() string {
	return "supercov: the active mutant passed its hit limit (SUPERCOV_MUTANT_TIMEOUT)"
}

func init() {
	if spec := os.Getenv(ActiveMutantEnv); spec != "" {
		if at := strings.LastIndexByte(spec, '#'); at > 0 {
			if index, err := strconv.ParseInt(spec[at+1:], 10, 32); err == nil && index >= 0 {
				strongFile, strongIndex = spec[:at], int32(index)
			}
		}
	}
	if limit, err := strconv.ParseInt(os.Getenv(MutantHitLimitEnv), 10, 64); err == nil && limit > 0 {
		strongHitLimit = limit
	}
}

// StrongSlot is the switch record of the file mutants.json keys `file`.
func StrongSlot(file string) *Strong {
	if file == strongFile {
		return &Strong{A: strongIndex}
	}
	return &Strong{A: -1}
}

// On says whether mutant `i` of this file is active, and counts the
// evaluation when it is.
func (s *Strong) On(i int32) bool {
	if s.A != i {
		return false
	}
	strongHit()
	return true
}

// Is says whether mutant `i` of this file is active, without counting.
func (s *Strong) Is(i int32) bool {
	return s.A == i
}

// MutantHits is how many times the active mutant's switch chose it.
func MutantHits() int64 {
	return strongHits.Load()
}

func strongHit() {
	if strongHits.Add(1) <= strongHitLimit {
		return
	}
	strongTimedOut.Do(func() {
		results := os.Getenv(StrongResultsEnv)
		if results == "" {
			return
		}
		file, err := os.OpenFile(results+".timeouts.jsonl", os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
		if err != nil {
			return
		}
		mutant := strconv.Quote(strongFile + "#" + strconv.Itoa(int(strongIndex)))
		_, _ = file.WriteString(`{"mutant":` + mutant + `,"attemptId":null,"pass":"main"}` + "\n")
		_ = file.Close()
	})
	panic(MutantTimeout{})
}

// Number is every type the arithmetic switches take: the operators they
// swap are defined on all of them.
type Number interface {
	~int | ~int8 | ~int16 | ~int32 | ~int64 |
		~uint | ~uint8 | ~uint16 | ~uint32 | ~uint64 | ~uintptr |
		~float32 | ~float64 | ~complex64 | ~complex128
}

// Integer is every type `%` is defined on.
type Integer interface {
	~int | ~int8 | ~int16 | ~int32 | ~int64 |
		~uint | ~uint8 | ~uint16 | ~uint32 | ~uint64 | ~uintptr
}

// The relational switches: `x OP y`, with OP's boundary mutant `b` and its
// negation `n` (-2 where it has no switch: a record's -1 means none is
// active, and must not match). Each operand is evaluated once,
// by the call, as it was by the operator.

// Lss is `x < y`; its boundary is `<=`, its negation `>=`.
func Lss[T cmp.Ordered](s *Strong, b, n int32, x, y T) bool {
	switch {
	case s.On(b):
		return x <= y
	case s.On(n):
		return x >= y
	}
	return x < y
}

// Leq is `x <= y`; its boundary is `<`, its negation `>`.
func Leq[T cmp.Ordered](s *Strong, b, n int32, x, y T) bool {
	switch {
	case s.On(b):
		return x < y
	case s.On(n):
		return x > y
	}
	return x <= y
}

// Gtr is `x > y`; its boundary is `>=`, its negation `<=`.
func Gtr[T cmp.Ordered](s *Strong, b, n int32, x, y T) bool {
	switch {
	case s.On(b):
		return x >= y
	case s.On(n):
		return x <= y
	}
	return x > y
}

// Geq is `x >= y`; its boundary is `>`, its negation `<`.
func Geq[T cmp.Ordered](s *Strong, b, n int32, x, y T) bool {
	switch {
	case s.On(b):
		return x > y
	case s.On(n):
		return x < y
	}
	return x >= y
}

// Add is `x + y`; its mutant `x - y`.
func Add[T Number](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x - y
	}
	return x + y
}

// Sub is `x - y`; both its mutants (INVERT_NEGATIVES and ARITHMETIC_BASE)
// are `x + y` (-2 for one without a switch).
func Sub[T Number](s *Strong, i, j int32, x, y T) T {
	if s.On(i) || s.On(j) {
		return x + y
	}
	return x - y
}

// Mul is `x * y`; its mutant `x / y`.
func Mul[T Number](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x / y
	}
	return x * y
}

// Quo is `x / y`; its mutant `x * y`.
func Quo[T Number](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x * y
	}
	return x / y
}

// Rem is `x % y`; its mutant `x * y`.
func Rem[T Integer](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x * y
	}
	return x % y
}

// Neg is `-x`; both its mutants are `+x`.
func Neg[T Number](s *Strong, i, j int32, x T) T {
	if s.On(i) || s.On(j) {
		return +x
	}
	return -x
}

// Pos is `+x`; its mutant `-x`.
func Pos[T Number](s *Strong, i int32, x T) T {
	if s.On(i) {
		return -x
	}
	return +x
}

// Pick chooses among a constant expression as written (`values[0]`) and its
// mutants (`values[k+1]` for mutant `indices[k]`): each is written whole, so
// the compiler evaluates every one as the constant it is.
func Pick[T any](s *Strong, indices []int32, values ...T) T {
	for k, i := range indices {
		if s.On(i) {
			return values[k+1]
		}
	}
	return values[0]
}

// And is `x & y`; its mutant `x | y`.
func And[T Integer](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x | y
	}
	return x & y
}

// Or is `x | y`; its mutant `x & y`.
func Or[T Integer](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x & y
	}
	return x | y
}

// Xor is `x ^ y`; its mutant `x & y`.
func Xor[T Integer](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x & y
	}
	return x ^ y
}

// AndNot is `x &^ y`; its mutant `x & y`.
func AndNot[T Integer](s *Strong, i int32, x, y T) T {
	if s.On(i) {
		return x & y
	}
	return x &^ y
}

// Shl is `x << n`; its mutant `x >> n`. The count may be any integer type.
func Shl[T, U Integer](s *Strong, i int32, x T, n U) T {
	if s.On(i) {
		return x >> n
	}
	return x << n
}

// Shr is `x >> n`; its mutant `x << n`.
func Shr[T, U Integer](s *Strong, i int32, x T, n U) T {
	if s.On(i) {
		return x << n
	}
	return x >> n
}
