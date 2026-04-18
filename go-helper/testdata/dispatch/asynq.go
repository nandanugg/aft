package dispatch

// Asynq-style task registration: HandleFunc(taskType, handlerFunc)

type ServeMux struct{}

func (m *ServeMux) HandleFunc(pattern string, handler func() error) {}

func HandleTaskA() error { return nil }
func HandleTaskB() error { return nil }

func RegisterHandlers(mux *ServeMux) {
	mux.HandleFunc("TypeTaskA", HandleTaskA)
	mux.HandleFunc("TypeTaskB", HandleTaskB)
}
