package lifecycle

import "context"

// InitConsumer has an event handler slot (handler) and a lifecycle hook slot (closeFn).
func InitConsumer(topic string, handler func(ctx context.Context, msg string) error, closeFn func() error) {}

func handle(ctx context.Context, msg string) error { return nil }

// Close is a lifecycle hook, not a handler.
func Close() error { return nil }

func Run() {
	InitConsumer("topic-a", handle, Close)
}
