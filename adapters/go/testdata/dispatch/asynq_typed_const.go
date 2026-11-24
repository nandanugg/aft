package dispatch

// asynq_typed_const.go exercises Feature 2: constant resolution for typed-string
// constants and string(...) type-casts.
//
// The real asynq pattern uses a named TaskType type and passes string(TypeX)
// as the dispatch key. Our extractNearbyString previously missed these
// because it only matched bare *ssa.Const, not *ssa.Convert of a *ssa.Const.

// TaskType is a named string type — simulates a project's own asynq task type.
type TaskType string

const (
	TypeOrderSync   TaskType = "order_sync:order_id"
	TypeOrderCancel TaskType = "order_cancel:order_id"
)

// TypedMux is a minimal stand-in for asynq.ServeMux.
type TypedMux struct{}

func (m *TypedMux) HandleFunc(pattern string, handler func() error) {}

func HandleOrderSyncTask() error   { return nil }
func HandleOrderCancelTask() error { return nil }

// RegisterTypedHandlers uses string(...) casts — the pattern we want to resolve.
func RegisterTypedHandlers(mux *TypedMux) {
	mux.HandleFunc(string(TypeOrderSync), HandleOrderSyncTask)
	mux.HandleFunc(string(TypeOrderCancel), HandleOrderCancelTask)
}
