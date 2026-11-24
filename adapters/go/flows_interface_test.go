package main

import "testing"

// A value passed to a call through an interface flows into the parameter of each non-mock method
// of the repository that can run for it, with tag interface (point 2, case A13: a service method
// that looks an order up passes its parameter to s.orderStore.Find).
func TestFlowsFollowAnInterfaceCallIntoTheConcreteMethod(t *testing.T) {
	res := analyzeModule(t, "ifaceflow")
	from := at(t, "ifaceflow", "svc/svc.go", "Find(id", "id")
	into := at(t, "ifaceflow", "store/store.go", "(s *storer) Get(ctx string, id", "id")
	found := false
	for _, fact := range res.facts {
		if fact.Kind != "flows" || fact.Subject != from {
			continue
		}
		if fact.Object.File == "mocks/Storer.go" {
			t.Errorf("a flow into a mock: %+v", fact)
		}
		if fact.Object == into {
			found = true
			if fact.Label != "parameter" || !hasTag(fact.Tags, "interface") {
				t.Errorf("flow into storer.Get: label %q tags %v, want parameter with tag interface", fact.Label, fact.Tags)
			}
		}
	}
	if !found {
		t.Fatalf("no flow from svc.Find's id into storer.Get's id; flows from it: %+v", flowsFrom(res, from))
	}
}

// err.Error() on the universe `error` interface is not followed into the Error methods of the
// repository (site records leave it out too): it would link every error value to unrelated types.
func TestFlowsDoNotFollowTheUniverseErrorInterface(t *testing.T) {
	res := analyzeModule(t, "ifaceflow")
	from := at(t, "ifaceflow", "svc/svc.go", "Describe(err", "err")
	if got := flowsFrom(res, from); len(got) != 0 {
		t.Fatalf("flows from err in Describe: %+v, want none", got)
	}
}

func flowsFrom(res *result, p position) []factRecord {
	var out []factRecord
	for _, fact := range res.facts {
		if fact.Kind == "flows" && fact.Subject == p {
			out = append(out, fact)
		}
	}
	return out
}
