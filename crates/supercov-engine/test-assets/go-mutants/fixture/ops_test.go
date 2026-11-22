package fixture

import "testing"

func TestOps(t *testing.T) {
	if c, _, s, _ := Ops(3, 4, 1, 2, "x"); c == 0 || s != "x!" {
		t.Fatal(c, s)
	}
	if Loops([]int{1, -1, 2, 200, 0, 3}) == 0 {
		t.Fatal()
	}
	if Closure(3)() != 4 || Sum(1, 2) != 3 || KB != 1024 || table[0] != 0 || started == 0 {
		t.Fatal()
	}
	done, values := make(chan struct{}), make(chan int)
	go func() { values <- 1; values <- -1; close(done) }()
	if Select(done, values) != 1 {
		t.Fatal()
	}
}
