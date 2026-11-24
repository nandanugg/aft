package dispatch

// interface_field_dispatch.go exercises interface-typed struct field calls
// with multiple concrete implementations. The helper must emit one edge per
// reachable concrete target rather than collapsing them by method name.

type FieldWorker interface {
	Handle(int) int
}

type AlphaWorker struct{}

func (AlphaWorker) Handle(v int) int { return v + 1 }

type BetaWorker struct{}

func (BetaWorker) Handle(v int) int { return v + 2 }

type FieldRunner struct {
	worker FieldWorker
}

func (r FieldRunner) Run(v int) int {
	return r.worker.Handle(v)
}

func UseFieldRunner() int {
	first := FieldRunner{worker: AlphaWorker{}}
	second := FieldRunner{worker: BetaWorker{}}
	return first.Run(1) + second.Run(2)
}
