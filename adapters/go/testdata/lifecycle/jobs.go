package lifecycle

import "context"

type Mux struct{}

func (m *Mux) HandleFunc(pattern string, h func(ctx context.Context, payload string) error) {}

func work1(ctx context.Context) error { return nil }

func work2(ctx context.Context) error { return nil }

// Setup registers a job whose handler is a literal that does more than forward.
func Setup(m *Mux) {
	m.HandleFunc("job:multi", func(ctx context.Context, payload string) error {
		if err := work1(ctx); err != nil {
			return err
		}
		return work2(ctx)
	})
}
