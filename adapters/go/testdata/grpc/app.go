package app

import "example.com/grpc/pb"

type handler struct {
	pb.UnimplementedOrderServiceServer
}

func (handler) Create() error { return nil }
func (handler) Cancel() error { return nil }

func New() pb.OrderServiceServer { return handler{} }

// RegisterConcrete passes the concrete type.
func RegisterConcrete() {
	pb.RegisterOrderServiceServer(nil, handler{})
}

// RegisterInterface passes a value of the interface type: every implementer in the repository counts.
func RegisterInterface() {
	srv := New()
	pb.RegisterOrderServiceServer(nil, srv)
}
