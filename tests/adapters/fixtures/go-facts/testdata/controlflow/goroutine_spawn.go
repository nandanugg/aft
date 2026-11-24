package controlflow

func worker() {
	// worker logic
}

// SpawnWorker launches worker in a goroutine.
func SpawnWorker() {
	go worker()
}
