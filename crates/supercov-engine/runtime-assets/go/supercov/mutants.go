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
	if int(id) < len(hits) {
		slot := &hits[id]
		for {
			old := atomic.LoadUint32(slot)
			if old&seen == seen || atomic.CompareAndSwapUint32(slot, old, old|seen) {
				break
			}
		}
	}
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
