package main

import "testing"

// Point 2, case A24/A24b: kafka.InitConsumer(..., handler, app.Close) must not
// register the close hook, and an asynq job whose handler literal does more than forward must still
// give a fact for its key.
func TestRegistersSkipsLifecycleHooksAndKeepsLabelledLiterals(t *testing.T) {
	res := analyzeModule(t, "lifecycle")
	byLabel := map[string][]factRecord{}
	for _, fact := range res.facts {
		if fact.Kind == "registers" {
			byLabel[fact.Label] = append(byLabel[fact.Label], fact)
		}
	}
	topic := byLabel["topic-a"]
	if len(topic) != 1 || topic[0].ObjectName != "handle" {
		t.Errorf("topic-a: want exactly the handler handle, got %+v", topic)
	}
	job := byLabel["job:multi"]
	literal := at(t, "lifecycle", "jobs.go", `m.HandleFunc("job:multi", func(`, "func(")
	if len(job) != 1 || job[0].ObjectName != "func literal" || job[0].Object != literal {
		t.Errorf("job:multi: want the literal at %+v, got %+v", literal, job)
	}
}
