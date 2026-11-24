package main

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// Helpers shared by the tests. Every test runs the adapter on a small Go module under testdata/
// (or one made in a temporary directory) and reads the records back.

func analyzeModule(t *testing.T, name string) *result {
	t.Helper()
	dir, err := filepath.Abs(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	return analyzeDir(t, dir)
}

func analyzeDir(t *testing.T, dir string) *result {
	t.Helper()
	res, err := analyze(dir, nil)
	if err != nil {
		t.Fatalf("analyze %s: %v", dir, err)
	}
	return res
}

// at returns the position of the last occurrence of sub inside the first occurrence of text in a
// file of the module. Example: at(t, "closures", "app.go", "run := func() { Work(", "(") is the open
// parenthesis of the call to Work.
func at(t *testing.T, module, file, text, sub string) position {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", module, file))
	if err != nil {
		t.Fatal(err)
	}
	src := string(data)
	start := strings.Index(src, text)
	if start < 0 {
		t.Fatalf("%s/%s: text %q not found", module, file, text)
	}
	if !strings.Contains(text, sub) {
		t.Fatalf("%q is not inside %q", sub, text)
	}
	offset := start + strings.LastIndex(text, sub)
	line := 1 + strings.Count(src[:offset], "\n")
	col := offset - (strings.LastIndex(src[:offset], "\n") + 1) + 1
	return position{File: file, Line: line, Col: col}
}

// siteAt finds the site record at exactly this position.
func siteAt(t *testing.T, res *result, p position) *siteRecord {
	t.Helper()
	for i := range res.sites {
		s := &res.sites[i]
		if s.File == p.File && s.Line == p.Line && s.Col == p.Col {
			return s
		}
	}
	return nil
}

func mustSite(t *testing.T, res *result, p position) *siteRecord {
	t.Helper()
	s := siteAt(t, res, p)
	if s == nil {
		t.Fatalf("no site at %v", p)
	}
	return s
}

// kindAt returns the kind of the site's target at p, or "" when there is none.
func kindAt(s *siteRecord, p position) string {
	for _, tg := range s.Targets {
		if tg.File == p.File && tg.Line == p.Line && tg.Col == p.Col {
			return tg.Kind
		}
	}
	return ""
}

func hasTarget(s *siteRecord, p position) bool {
	for _, tg := range s.Targets {
		if tg.File == p.File && tg.Line == p.Line && tg.Col == p.Col {
			return true
		}
	}
	return false
}

func hasTag(tags []string, want string) bool {
	for _, tag := range tags {
		if tag == want {
			return true
		}
	}
	return false
}

func targetNames(s *siteRecord) []string {
	var names []string
	for _, tg := range s.Targets {
		names = append(names, tg.File+":"+strconv.Itoa(tg.Line)+" "+tg.Name+" ("+tg.Kind+")")
	}
	return names
}
