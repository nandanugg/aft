package via

import (
	"context"

	"example.com/via/queue"
)

func refund(ctx context.Context) error { return nil }

func payout(ctx context.Context) error { return nil }

func report(ctx context.Context) error { return nil }

// register hands a handler to a function of the same package.
func register(h func(ctx context.Context) error) {}

func Setup(m *queue.ServeMux) {
	m.HandleFunc("job:refund", refund)
	queue.Handle("job:payout", payout)
	register(report)
}
