package callgraph

// Case A: bare package-level function called unqualified from the same file.
// `callers` / `trace_to` should surface the call site at line in barePkgCaller.
func barePkgTarget(x int) int {
	return x + 1
}

func barePkgCaller(x int) int {
	return barePkgTarget(x)
}

// Case B: method on a concrete receiver called via a local var of that type.
// `s.concreteMethod(...)` where `s` is typed `*concreteSvc` should resolve
// to `func (s *concreteSvc) concreteMethod(...)`.
type concreteSvc struct{}

func (s *concreteSvc) concreteMethod(x int) int {
	return x * 2
}

func concreteMethodCaller(x int) int {
	s := &concreteSvc{}
	return s.concreteMethod(x)
}

// Case C: interface-method dispatch. A variable typed as an interface should
// resolve to every implementation of that interface's method.
type Doer interface {
	Do(x int) int
}

type doerA struct{}

func (a *doerA) Do(x int) int { return x + 10 }

type doerB struct{}

func (b *doerB) Do(x int) int { return x + 100 }

func interfaceCaller(d Doer, x int) int {
	return d.Do(x)
}

// Case D: field-write origin. Writing to a field of a tracked value should
// register a hop from the RHS into the LHS field.
type Message struct {
	Account string
}

func fieldWriteCase(m *Message, name string) {
	m.Account = name
}
