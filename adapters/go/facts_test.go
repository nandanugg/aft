package main

import "testing"

func TestGRPCRegistrationGivesOneFactPerMethodWithTheServiceName(t *testing.T) {
	res := analyzeModule(t, "grpc")
	create := at(t, "grpc", "app.go", "func (handler) Create", "Create")
	cancel := at(t, "grpc", "app.go", "func (handler) Cancel", "Cancel")

	for _, caller := range []struct{ name, call string }{
		{"RegisterConcrete", "pb.RegisterOrderServiceServer(nil, handler{}"},
		{"RegisterInterface", "pb.RegisterOrderServiceServer(nil, srv"},
	} {
		line := at(t, "grpc", "app.go", caller.call, "pb.").Line
		got := map[string]position{}
		for _, f := range factsOfKind(res, "registers") {
			if f.Subject.Line == line {
				got[f.Label] = f.Object
			}
		}
		if got["/orders.OrderService/Create"] != create || got["/orders.OrderService/Cancel"] != cancel || len(got) != 2 {
			t.Errorf("%s: registers facts %v, want Create at %v and Cancel at %v under the descriptor's service name", caller.name, got, create, cancel)
		}
	}
}
