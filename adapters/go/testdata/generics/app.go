package app

type Set[T comparable] struct{ items map[T]bool }

func (s *Set[T]) Add(v T) { s.items[v] = true }

func Map[T, U any](xs []T, f func(T) U) []U {
	var out []U
	for _, x := range xs {
		out = append(out, f(x))
	}
	return out
}

func UseGeneric() {
	s := &Set[int]{items: map[int]bool{}}
	s.Add(1)
	_ = Map[int, string]([]int{1}, func(i int) string { return "" })
	_ = Map([]int{1}, func(i int) int { return i })
}

type Getter interface{ Get() int }

// Box is a generic type that has Get: the adapter cannot test generic types, so it must not claim to
// know every implementer of Getter.
type Box[T any] struct{ v T }

func (b *Box[T]) Get() int { return 0 }

func UseGetter(g Getter) { g.Get() }
