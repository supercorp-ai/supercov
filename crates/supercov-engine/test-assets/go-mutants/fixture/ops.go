package fixture

import "time"

// Every token Gremlins mutates, in the places Go puts them.

const (
	KB = 1 << (10 * (iota + 1))
	MB
)

const limit = 100 - 1

var table [limit + 1]int

var started = time.Second * 2

type Number interface {
	~int | ~float64
}

func Sum[T Number](values ...T) T {
	var total T
	for _, value := range values {
		total += value
	}
	return total
}

func Ops(a, b int, x, y float64, s string) (int, float64, string, bool) {
	c := a + b - a*b/2%3
	c += a
	c -= b
	c *= 2
	c /= 3
	c %= 5
	c &= 7
	c |= 8
	c ^= 9
	c <<= 1
	c >>= 1
	c &^= 3
	p := &c
	*p = ^*p & a | b ^ a &^ 1 << 2 >> 1
	z := -x + +y
	n := -a
	ok := a < b || a <= b && a > b || a >= b
	same := a == b && a != b
	s += "!"
	c++
	n--
	return c + n, z / y, s, ok && !same
}

func Loops(values []int) (count int) {
outer:
	for i := 0; i < len(values); i++ {
		switch {
		case values[i] < 0:
			continue outer
		case values[i] == 0:
			break outer
		case values[i] > 100:
			break
		}
		for j := range values {
			if j > i {
				break
			}
			if j == i {
				continue
			}
			count++
		}
	}
	return count
}

func Closure(n int) func() int {
	return func() int {
		n--
		return n * 2
	}
}

func Select(done chan struct{}, values chan int) (total int) {
	for {
		select {
		case <-done:
			return total
		case value := <-values:
			if value < 0 {
				break
			}
			total += value
		}
	}
}
