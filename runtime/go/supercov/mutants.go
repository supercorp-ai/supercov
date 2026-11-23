//go:build go1.21

package supercov

// Weak mutation's value probes (docs/design/mutants.md, Go): what the
// operands of a comparison were, so a boundary mutant (`<` to `<=`) is
// infected only where they were equal.

import (
	"cmp"
	"sync/atomic"
)

// Cmp is `x OP y` for OP `<`, `<=`, `>`, `>=` (`op` 0 to 3), recording in
// slot `id` how its operands related: 1 less, 2 equal, 4 greater, 8 neither
// (a NaN). Each operand is evaluated once, by the call, as by the operator.
func Cmp[T cmp.Ordered](id uint32, op uint8, x, y T) bool {
	var seen uint32
	switch {
	case x < y:
		seen = 1
	case x == y:
		seen = 2
	case x > y:
		seen = 4
	default:
		seen = 8
	}
	record(id, seen)
	switch op {
	case 0:
		return x < y
	case 1:
		return x <= y
	case 2:
		return x > y
	}
	return x >= y
}

// record ORs `seen` into slot `id`, losing nothing to another goroutine's
// store into it.
func record(id uint32, seen uint32) {
	if int(id) >= len(hits) {
		return
	}
	slot := &hits[id]
	for {
		old := atomic.LoadUint32(slot)
		if old&seen == seen || atomic.CompareAndSwapUint32(slot, old, old|seen) {
			return
		}
	}
}

// outcome records whether alternative `alternative` (0 the operator's swap,
// 1 a removed self-assignment) gave the value the program did.
func outcome(id uint32, alternative uint, same bool) {
	bit := uint32(32)
	if same {
		bit = 16
	}
	record(id, bit<<(2*alternative))
}

// Arith is `x OP y` for OP `+ - * / % & | ^ &^` (`op` 0 to 8), recording
// whether Gremlins' swap of OP gave the same value. The program's own
// operation runs first, so it panics where the program does.
func Arith[T Integer](id uint32, op uint8, x, y T) T {
	var result T
	switch op {
	case 0:
		result = x + y
	case 1:
		result = x - y
	case 2:
		result = x * y
	case 3:
		result = x / y
	case 4:
		result = x % y
	case 5:
		result = x & y
	case 6:
		result = x | y
	case 7:
		result = x ^ y
	default:
		result = x &^ y
	}
	alternative, same := swapped(op, x, y)
	outcome(id, 0, same && alternative == result)
	return result
}

// swapped is `x OP' y` for OP's swap, and false where it panics (a division
// by zero): a value that differs.
func swapped[T Integer](op uint8, x, y T) (T, bool) {
	switch op {
	case 0:
		return x - y, true
	case 1:
		return x + y, true
	case 2:
		if y == 0 {
			return 0, false
		}
		return x / y, true
	case 3, 4:
		return x * y, true
	case 5:
		return x | y, true
	}
	return x & y, true
}

// Shift is `x << n` or `x >> n` (`op` 0, 1).
func Shift[T, U Integer](id uint32, op uint8, x T, n U) T {
	result, alternative := x<<n, x>>n
	if op == 1 {
		result, alternative = alternative, result
	}
	outcome(id, 0, alternative == result)
	return result
}

// Unary is `-x` or `+x` (`op` 0, 1); either's mutant is the other.
func Unary[T Integer](id uint32, op uint8, x T) T {
	result := -x
	if op == 1 {
		result = x
	}
	outcome(id, 0, -x == x)
	return result
}

// Assign is the right-hand side `y` of `x OP= y`, returned as it was,
// recording whether `x OP'= y` (OP's swap; `%=` stays) and `x = y` would
// have stored what `x OP= y` stores.
func Assign[T Integer](id uint32, op uint8, x, y T) T {
	var result T
	switch op {
	case 0:
		result = x + y
	case 1:
		result = x - y
	case 2:
		result = x * y
	case 3:
		result = x / y
	case 4:
		result = x % y
	case 5:
		result = x & y
	case 6:
		result = x | y
	case 7:
		result = x ^ y
	default:
		result = x &^ y
	}
	alternative, same := assigned(op, x, y)
	outcome(id, 0, same && alternative == result)
	outcome(id, 1, y == result)
	return y
}

// assigned is `x OP'= y`'s stored value for INVERT_ASSIGNMENTS' and
// INVERT_BWASSIGN's swaps.
func assigned[T Integer](op uint8, x, y T) (T, bool) {
	switch op {
	case 0:
		return x - y, true
	case 1:
		return x + y, true
	case 2:
		if y == 0 {
			return 0, false
		}
		return x / y, true
	case 3:
		return x * y, true
	case 4:
		return x % y, true
	case 5:
		return x | y, true
	}
	return x & y, true
}

// AssignShift is the count `n` of `x <<= n` or `x >>= n` (`op` 0, 1),
// recording whether the other shift and `x = n` would have stored the same.
func AssignShift[T, U Integer](id uint32, op uint8, x T, n U) U {
	result, alternative := x<<n, x>>n
	if op == 1 {
		result, alternative = alternative, result
	}
	outcome(id, 0, alternative == result)
	outcome(id, 1, T(n) == result)
	return n
}
