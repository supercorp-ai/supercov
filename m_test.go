package m

import "testing"

func TestF(t *testing.T) {
	if F(true, true) != 1 {
		t.Fatal("bad")
	}
}
