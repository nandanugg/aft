// Package pb stands in for generated gRPC code.
package pb

type ServiceDesc struct{ ServiceName string }

var OrderService_ServiceDesc = ServiceDesc{ServiceName: "orders.OrderService"}

type OrderServiceServer interface {
	Create() error
	Cancel() error
	mustEmbedUnimplementedOrderServiceServer()
}

type UnimplementedOrderServiceServer struct{}

func (UnimplementedOrderServiceServer) mustEmbedUnimplementedOrderServiceServer() {}

func RegisterOrderServiceServer(registrar any, srv OrderServiceServer) {}
