package queue

import "context"

type ServeMux struct{}

// HandleFunc is like asynq's (*ServeMux).HandleFunc.
func (m *ServeMux) HandleFunc(pattern string, h func(ctx context.Context) error) {}

// Handle is a plain function of another package.
func Handle(pattern string, h func(ctx context.Context) error) {}
