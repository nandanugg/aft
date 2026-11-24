package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Tests for call sites on small modules under testdata/. Each test says what the user asks aft.

func TestGenericsMapToTheDeclaredName(t *testing.T) {
	res := analyzeModule(t, "generics")
	add := at(t, "generics", "app.go", "func (s *Set[T]) Add", "Add")
	mapDecl := at(t, "generics", "app.go", "func Map[T, U any]", "Map")

	site := mustSite(t, res, at(t, "generics", "app.go", "s.Add(", "("))
	if !hasTarget(site, add) || site.Targets[0].Kind != kindConcrete {
		t.Errorf("s.Add targets %v, want the generic method declaration %v", targetNames(site), add)
	}
	explicit := mustSite(t, res, at(t, "generics", "app.go", "Map[int, string](", "("))
	if !hasTarget(explicit, mapDecl) || explicit.Name != "Map" {
		t.Errorf("explicit instantiation: name %q targets %v, want Map at %v", explicit.Name, targetNames(explicit), mapDecl)
	}
	inferred := mustSite(t, res, at(t, "generics", "app.go", "= Map(", "("))
	if !hasTarget(inferred, mapDecl) {
		t.Errorf("inferred instantiation targets %v, want Map at %v", targetNames(inferred), mapDecl)
	}
	if s := siteAt(t, res, at(t, "generics", "app.go", "f(x)", "(")); s != nil {
		t.Errorf("call through the function value f has a site: %v", s)
	}
}

func TestInterfaceCallIsLeftOutWhenAGenericTypeMayImplementIt(t *testing.T) {
	res := analyzeModule(t, "generics")
	if s := siteAt(t, res, at(t, "generics", "app.go", "g.Get(", "(")); s != nil {
		t.Errorf("g.Get() has a site with targets %v; a generic type (Box) may also implement Getter, so the list cannot be complete", targetNames(s))
	}
}

func TestEmbeddedMethodsReachTheDeclaredMethod(t *testing.T) {
	res := analyzeModule(t, "embedded")
	innerM := at(t, "embedded", "app.go", "func (Inner) M", "M")
	baseSpeak := at(t, "embedded", "app.go", "func (Base) Speak", "Speak")

	promoted := mustSite(t, res, at(t, "embedded", "app.go", "o.M(", "("))
	if !hasTarget(promoted, innerM) || promoted.Targets[0].Kind != kindConcrete {
		t.Errorf("promoted method targets %v, want Inner.M at %v", targetNames(promoted), innerM)
	}
	speakerSpeak := at(t, "embedded", "app.go", "Speak() string", "Speak")
	viaInterface := mustSite(t, res, at(t, "embedded", "app.go", "s.Speak(", "("))
	if kindAt(viaInterface, baseSpeak) != kindInterface {
		t.Errorf("interface call targets %v, want Base.Speak (reached through Dog) at %v", targetNames(viaInterface), baseSpeak)
	}
	if kindAt(viaInterface, speakerSpeak) != kindInterfaceMethod {
		t.Errorf("interface call targets %v, want the interface method itself at %v", targetNames(viaInterface), speakerSpeak)
	}
	viaEmbedding := mustSite(t, res, at(t, "embedded", "app.go", "d.Speak(", "("))
	if !hasTarget(viaEmbedding, baseSpeak) {
		t.Errorf("d.Speak targets %v, want Base.Speak at %v", targetNames(viaEmbedding), baseSpeak)
	}
	embeddedInterface := mustSite(t, res, at(t, "embedded", "app.go", "w.Speak(", "("))
	if kindAt(embeddedInterface, baseSpeak) != kindInterface {
		t.Errorf("call on a struct that embeds the interface: targets %v, want an interface call that reaches Base.Speak", targetNames(embeddedInterface))
	}
}

func TestClosuresAndTags(t *testing.T) {
	res := analyzeModule(t, "closures")
	work := at(t, "closures", "app.go", "func Work", "Work")
	other := at(t, "closures", "app.go", "func Other", "Other")

	inClosure := mustSite(t, res, at(t, "closures", "app.go", "run := func() { Work(", "("))
	if !hasTarget(inClosure, work) || len(inClosure.Tags) != 0 {
		t.Errorf("call inside a closure: targets %v tags %v, want Work and no tags", targetNames(inClosure), inClosure.Tags)
	}
	if s := siteAt(t, res, at(t, "closures", "app.go", "run()", "(")); s != nil {
		t.Errorf("run() calls a function value but has a site: %v", s)
	}
	inGoroutine := mustSite(t, res, at(t, "closures", "app.go", "go func() { Work(", "("))
	if !hasTag(inGoroutine.Tags, "in_goroutine") || !hasTarget(inGoroutine, work) {
		t.Errorf("call in a goroutine literal: tags %v targets %v", inGoroutine.Tags, targetNames(inGoroutine))
	}
	inDefer := mustSite(t, res, at(t, "closures", "app.go", "defer func() { Other(", "("))
	if !hasTag(inDefer.Tags, "in_defer") || !hasTarget(inDefer, other) {
		t.Errorf("call in a deferred literal: tags %v targets %v", inDefer.Tags, targetNames(inDefer))
	}
}

func TestLoopTagOnACallInsideALoop(t *testing.T) {
	res := analyzeModule(t, "closures")
	site := mustSite(t, res, at(t, "closures", "app.go", "for range items {\n\t\tfunc() { Work(", "("))
	if !hasTag(site.Tags, "in_loop") {
		t.Errorf("tags %v, want in_loop", site.Tags)
	}
}

func TestColumnCountsBytes(t *testing.T) {
	res := analyzeModule(t, "closures")
	p := at(t, "closures", "app.go", `_ = "héllo, 世界"; Work(`, "(")
	site := mustSite(t, res, p)
	if site.Name != "Work" {
		t.Errorf("site name %q, want Work", site.Name)
	}
}

func TestImportedInterfaceReachesTheRepositoryTypesAndMarksMocks(t *testing.T) {
	res := analyzeModule(t, "imported")
	handlerServe := at(t, "imported", "app.go", "func (handler) ServeHTTP", "ServeHTTP")
	mockServe := position{File: "mocks/handler.go", Line: 7, Col: 22}

	site := mustSite(t, res, at(t, "imported", "app.go", "h.ServeHTTP(", "("))
	if !hasTarget(site, handlerServe) {
		t.Errorf("targets %v, want the repository's ServeHTTP at %v", targetNames(site), handlerServe)
	}
	var mock *target
	for i := range site.Targets {
		if site.Targets[i].File == "mocks/handler.go" {
			mock = &site.Targets[i]
		}
	}
	if mock == nil || mock.Line != mockServe.Line || !hasTag(mock.Tags, "mock") {
		t.Errorf("targets %v, want the mock ServeHTTP tagged mock", targetNames(site))
	}
	for _, tg := range site.Targets {
		if tg.File != "mocks/handler.go" && hasTag(tg.Tags, "mock") {
			t.Errorf("target %v has the mock tag but is not in mocks/", tg)
		}
	}
	if len(site.External) != 1 || site.External[0].Package != "net/http" || site.External[0].Name != "ServeHTTP" {
		t.Errorf("external %v, want the net/http interface method", site.External)
	}
	if s := siteAt(t, res, at(t, "imported", "app.go", "err.Error(", "(")); s != nil {
		t.Errorf("error.Error() dispatch must stay unresolved, got %v", s)
	}
}

func TestExternalLabelsNameThePackageAndModule(t *testing.T) {
	res := analyzeModule(t, "external")
	std := mustSite(t, res, at(t, "external", "app.go", "strings.ToUpper(", "("))
	if len(std.Targets) != 0 || len(std.External) != 1 || std.External[0] != (externalLabel{Name: "ToUpper", Package: "strings"}) {
		t.Errorf("standard library call: targets %v external %v", std.Targets, std.External)
	}
	dep := mustSite(t, res, at(t, "external", "app.go", "astutil.Unparen(", "("))
	want := externalLabel{Name: "Unparen", Package: "golang.org/x/tools/go/ast/astutil", Module: "golang.org/x/tools@v0.28.0"}
	if len(dep.External) != 1 || dep.External[0] != want {
		t.Errorf("dependency call: external %v, want %v", dep.External, want)
	}
}

func TestMissingModuleGivesAPartialRun(t *testing.T) {
	dir := t.TempDir()
	write := func(name, content string) {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write("go.mod", "module example.com/mm\n\ngo 1.22\n\nrequire example.com/doesnotexist v1.0.0\n")
	write("ok/ok.go", "package ok\n\nfunc A() int { return B() }\n\nfunc B() int { return 1 }\n")
	write("broken/broken.go", "package broken\n\nimport \"example.com/doesnotexist/x\"\n\nfunc C() { x.D() }\n")

	res := analyzeDir(t, dir)
	if len(res.unanalysed) != 1 || res.unanalysed[0].Unit != "example.com/mm/broken" {
		t.Fatalf("unanalysed %+v, want only example.com/mm/broken", res.unanalysed)
	}
	if got := res.unanalysed[0].Files; len(got) != 1 || got[0] != "broken/broken.go" {
		t.Errorf("unanalysed files %v, want broken/broken.go", got)
	}
	if res.unanalysed[0].Reason == "" {
		t.Error("unanalysed record has no reason")
	}
	call := mustSite(t, res, position{File: "ok/ok.go", Line: 3, Col: 24})
	if !hasTarget(call, position{File: "ok/ok.go", Line: 5, Col: 6}) {
		t.Errorf("the package that type-checked is still analysed: B() targets %v", targetNames(call))
	}
	for _, rec := range res.analysed {
		for _, f := range rec.Files {
			if f == "broken/broken.go" {
				t.Error("a package that did not type-check is listed as analysed")
			}
		}
	}
}

func TestEndRecordCountsMatchTheRecords(t *testing.T) {
	res := analyzeModule(t, "imported")
	var out bytes.Buffer
	if err := res.write(&out); err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSuffix(out.String(), "\n"), "\n")
	if !strings.HasPrefix(lines[0], `{"type":"header","contract":1,"language":"go"`) {
		t.Errorf("first line is not the header: %q", lines[0])
	}
	var end endRecord
	if err := json.Unmarshal([]byte(lines[len(lines)-1]), &end); err != nil || end.Type != "end" {
		t.Fatalf("last line is not the end record: %q (%v)", lines[len(lines)-1], err)
	}
	targets := 0
	for _, s := range res.sites {
		targets += len(s.Targets)
	}
	if end.Sites != len(res.sites) || end.Targets != targets || end.Facts != len(res.facts) {
		t.Errorf("end %+v does not match %d sites, %d targets, %d facts", end, len(res.sites), targets, len(res.facts))
	}
	if end.AnalysedFiles != 2 { // app.go and mocks/handler.go
		t.Errorf("analysed_files = %d, want 2", end.AnalysedFiles)
	}
}
