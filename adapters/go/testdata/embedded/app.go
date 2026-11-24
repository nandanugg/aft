package app

type Inner struct{}

func (Inner) M() {}

type Outer struct{ Inner }

type Speaker interface{ Speak() string }

type Base struct{}

func (Base) Speak() string { return "" }

// Dog gets Speak from the embedded Base.
type Dog struct{ Base }

// Wrapper embeds the interface: it satisfies Speaker but has no Speak of its own.
type Wrapper struct{ Speaker }

func UseEmbedded(o Outer, s Speaker, d Dog, w Wrapper) {
	o.M()
	s.Speak()
	d.Speak()
	w.Speak()
}
