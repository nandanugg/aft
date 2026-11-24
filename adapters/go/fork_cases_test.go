package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"testing"
)

// The tests of the old fork helper (control flow, dispatch, impls, writes and the six data-flow
// patterns), carried over to contract v1. testdata/expected.json holds the expectations: they were
// written from the fork's own tests (backup/main-before-origin-rebase-20260705 and feat/go-vta-helper),
// not from this adapter's output. The fixtures in testdata/<module>/ are byte copies of the fork's.
//
// Rules (the same as tests/adapters/lib/facts_compare.py):
//   - a position matches on file and line; a different column is only noted;
//   - a case marked "optional" logs a warning instead of failing;
//   - a case marked "unsure" is information only.

type expectedFile struct {
	Cases []expectedCase `json:"cases"`
}

type expectedCase struct {
	ID     string `json:"id"`
	Kind   string `json:"kind"`
	Module string `json:"module"`
	From   string `json:"from"`

	Optional bool   `json:"optional"`
	Unsure   string `json:"unsure"`

	SubjectName    string    `json:"subject_name"`
	SubjectAltName string    `json:"subject_alt_name"`
	Subject        *position `json:"subject"`
	ObjectName     string    `json:"object_name"`
	Object         *position `json:"object"`
	Label          *string   `json:"label"`
	LabelAbsent    bool      `json:"label_absent"`
	Absent         bool      `json:"absent"`

	Count            *int     `json:"count"`
	MinCount         *int     `json:"min_count"`
	LabelContainsAny []string `json:"label_contains_any"`

	Site   *position `json:"site"`
	Target *position `json:"target"`
	Tags   []string  `json:"tags"`
}

func TestForkCases(t *testing.T) {
	data, err := os.ReadFile("testdata/expected.json")
	if err != nil {
		t.Fatal(err)
	}
	var expected expectedFile
	if err := json.Unmarshal(data, &expected); err != nil {
		t.Fatal(err)
	}

	results := map[string]*result{}
	for _, c := range expected.Cases {
		if results[c.Module] == nil {
			results[c.Module] = analyzeModule(t, c.Module)
		}
	}

	for _, c := range expected.Cases {
		c := c
		t.Run(c.ID+"_"+c.Kind, func(t *testing.T) {
			if problem := checkCase(c, results[c.Module]); problem != "" {
				switch {
				case c.Unsure != "":
					t.Logf("INFO (unsure case): %s", problem)
				case c.Optional:
					t.Logf("WARN (optional case): %s", problem)
				default:
					t.Errorf("%s\n  from: %s", problem, c.From)
				}
			}
		})
	}
}

func sameLine(a, b *position) bool {
	return a != nil && b != nil && a.File == b.File && a.Line == b.Line
}

func factsOfKind(res *result, kind string) []factRecord {
	var facts []factRecord
	for _, f := range res.facts {
		if f.Kind == kind {
			facts = append(facts, f)
		}
	}
	return facts
}

// checkCase returns "" when the result meets the case, else what is wrong.
func checkCase(c expectedCase, res *result) string {
	facts := factsOfKind(res, c.Kind)
	switch {
	case c.Count != nil && c.Kind != "site":
		if len(facts) != *c.Count {
			return countMessage(len(facts), c.Kind, *c.Count)
		}

	case c.Kind == "registers" && c.Absent:
		for _, f := range facts {
			if f.ObjectName == "" {
				return "a registers fact has no object name"
			}
		}

	case c.Kind == "registers":
		for _, f := range facts {
			subject, object := f.Subject, f.Object
			if f.ObjectName != c.ObjectName || !sameLine(&subject, c.Subject) || !sameLine(&object, c.Object) {
				continue
			}
			if c.LabelAbsent && f.Label == "" || !c.LabelAbsent && c.Label != nil && f.Label == *c.Label {
				return ""
			}
		}
		return "no registers fact " + c.SubjectName + " -> " + c.ObjectName + " with the expected label"

	case c.Kind == "implements":
		for _, f := range facts {
			subject, object := f.Subject, f.Object
			if f.SubjectName == c.SubjectName && f.ObjectName == c.ObjectName && sameLine(&subject, c.Subject) && sameLine(&object, c.Object) {
				return ""
			}
		}
		return "no implements fact " + c.SubjectName + " -> " + c.ObjectName

	case c.Kind == "writes" && c.Absent:
		for _, f := range facts {
			if f.ObjectName == c.ObjectName && f.SubjectName == c.SubjectName {
				return "a write that the old helper excluded was emitted"
			}
		}

	case c.Kind == "writes":
		for _, f := range facts {
			object := f.Object
			if f.ObjectName == c.ObjectName && (f.SubjectName == c.SubjectName || f.SubjectName == c.SubjectAltName) && sameLine(&object, c.Object) {
				return ""
			}
		}
		return "no writes fact " + c.SubjectName + " -> " + c.ObjectName

	case c.Kind == "returns" && c.MinCount != nil:
		n := 0
		for _, f := range facts {
			if f.SubjectName == c.SubjectName {
				n++
			}
		}
		if n < *c.MinCount {
			return countMessage(n, "returns facts of "+c.SubjectName, *c.MinCount)
		}

	case c.Kind == "returns":
		for _, f := range facts {
			if f.SubjectName != c.SubjectName {
				continue
			}
			for _, part := range c.LabelContainsAny {
				if strings.Contains(f.Label, part) {
					return ""
				}
			}
		}
		return "no returns label of " + c.SubjectName + " contains any of " + strings.Join(c.LabelContainsAny, ", ")

	case c.Kind == "flows":
		for _, f := range facts {
			if f.SubjectName == c.SubjectName && f.ObjectName == c.ObjectName {
				return ""
			}
		}
		return "no flows fact " + c.SubjectName + " -> " + c.ObjectName

	case c.Kind == "site_tags":
		for i := range res.sites {
			s := &res.sites[i]
			if sameLine(&position{File: s.File, Line: s.Line}, c.Site) && containsAll(s.Tags, c.Tags) {
				return ""
			}
		}
		return "no site at " + c.Site.File + ":" + strconv.Itoa(c.Site.Line) + " with tags " + strings.Join(c.Tags, ",")

	case c.Kind == "site" && c.Count != nil:
		n := 0
		for _, s := range res.sites {
			if (hasTag(s.Tags, "in_goroutine") || hasTag(s.Tags, "in_defer")) && len(s.Targets) > 0 {
				n++
			}
		}
		if n < *c.Count {
			return countMessage(n, "goroutine and defer sites with targets", *c.Count)
		}

	case c.Kind == "site":
		for i := range res.sites {
			s := &res.sites[i]
			if !sameLine(&position{File: s.File, Line: s.Line}, c.Site) {
				continue
			}
			for _, tg := range s.Targets {
				if sameLine(&position{File: tg.File, Line: tg.Line}, c.Target) {
					return ""
				}
			}
		}
		return "no site at " + c.Site.File + ":" + strconv.Itoa(c.Site.Line) + " reaching " + c.Target.File + ":" + strconv.Itoa(c.Target.Line)

	default:
		return "case kind " + c.Kind + " is not handled by the test"
	}
	return ""
}

func containsAll(have, want []string) bool {
	for _, w := range want {
		if !hasTag(have, w) {
			return false
		}
	}
	return true
}

func countMessage(n int, what string, want int) string {
	return fmt.Sprintf("%d %s, expected %d", n, what, want)
}
