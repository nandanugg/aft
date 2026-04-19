package dispatch

// asynq_typed_const.go exercises Feature 2: constant resolution for typed-string
// constants and string(...) type-casts.
//
// The real asynq pattern uses a named TaskType type and passes string(TypeX)
// as the dispatch key. Our extractNearbyString previously missed these
// because it only matched bare *ssa.Const, not *ssa.Convert of a *ssa.Const.

// TaskType is a named string type — simulates dpayAsynq.TaskType.
type TaskType string

const (
	TypeMerchantSettlement TaskType = "merchant_settlement:merchant_id"
	TypeRefundCallback     TaskType = "refund_callback:payment_id"
)

// TypedMux is a minimal stand-in for asynq.ServeMux.
type TypedMux struct{}

func (m *TypedMux) HandleFunc(pattern string, handler func() error) {}

func HandleMerchantSettlementTask() error { return nil }
func HandleRefundCallbackTask() error     { return nil }

// RegisterTypedHandlers uses string(...) casts — the pattern we want to resolve.
func RegisterTypedHandlers(mux *TypedMux) {
	mux.HandleFunc(string(TypeMerchantSettlement), HandleMerchantSettlementTask)
	mux.HandleFunc(string(TypeRefundCallback), HandleRefundCallbackTask)
}
