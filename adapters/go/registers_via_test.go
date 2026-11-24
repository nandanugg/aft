package main

import "testing"

// A `registers` fact says through which registration API the handler was handed over (field via).
func TestRegistersFactsNameTheRegistrationAPI(t *testing.T) {
	res := analyzeModule(t, "via")
	want := map[string]string{
		"refund": "(*queue.ServeMux).HandleFunc", // a method of a type from another package
		"payout": "queue.Handle",                 // a function of another package
		"report": "via.register",                 // a function of the same package
	}
	got := map[string]string{}
	for _, fact := range factsOfKind(res, "registers") {
		got[fact.ObjectName] = fact.Via
	}
	for handler, via := range want {
		if got[handler] != via {
			t.Errorf("handler %s: via %q, want %q (all: %v)", handler, got[handler], via, got)
		}
	}
}

func TestGRPCRegistrationFactsNameTheRegisterFunction(t *testing.T) {
	res := analyzeModule(t, "grpc")
	facts := factsOfKind(res, "registers")
	if len(facts) == 0 {
		t.Fatal("no registers facts in the grpc module")
	}
	for _, fact := range facts {
		if fact.Via != "pb.RegisterOrderServiceServer" {
			t.Errorf("%s: via %q, want pb.RegisterOrderServiceServer", fact.ObjectName, fact.Via)
		}
	}
}

func TestRegistersLiteralHandlerHasVia(t *testing.T) {
	res := analyzeModule(t, "lifecycle")
	for _, fact := range factsOfKind(res, "registers") {
		if fact.Label == "job:multi" && fact.Via != "(*lifecycle.Mux).HandleFunc" {
			t.Errorf("job:multi: via %q", fact.Via)
		}
	}
}
