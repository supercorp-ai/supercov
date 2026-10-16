//go:build go1.18

package supercov

// CV is C for a decision used as a value -- `ok := a && b` -- where the
// operand's own boolean type has to survive the wrapper: `type Flag bool` is
// not assignable from `bool`, so converting would change what the
// expression is.
func CV[T ~bool](id uint32, index uint8, value T) T {
	C(id, index, bool(value))
	return value
}

// BV is BD for a decision used as a value, returning the expression with
// the type it had.
func BV[T ~bool](whenTrue, whenFalse, id uint32, value T) T {
	BD(whenTrue, whenFalse, id, bool(value))
	return value
}
