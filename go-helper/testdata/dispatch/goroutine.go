package dispatch

// Goroutine launch patterns

func workerLoop() {}
func cleanup()    {}
func process()    {}

func StartWorkers() {
	go workerLoop()
	go process()
}

func WithDefer() {
	defer cleanup()
	// do work
}
