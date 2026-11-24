package main

import (
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"path/filepath"
	"sort"
	"strings"
	"sync"

	"golang.org/x/tools/go/packages"
)

// rootPackage is one package of the repository (the packages that `go list ./...` names).
type rootPackage struct {
	pkg *packages.Package
	// typeChecked is true when the package has no error. Only such packages are analysed.
	typeChecked bool
	// files are the syntax trees of the package files that live inside the root, with their
	// root-relative paths in rels (same order).
	files []*ast.File
	rels  []string
	// analysedRels are the root-relative paths of every compiled file of the package.
	analysedRels []string
}

// project is the loaded repository.
type project struct {
	rootDirs   []string // the root as given, and the same path with symlinks resolved
	fset       *token.FileSet
	roots      []*rootPackage // sorted by package path
	rootByPath map[string]*rootPackage
	moduleOf   map[string]*packages.Module // package path -> module, for every loaded package
	relMu      sync.Mutex                  // the parse hook runs in many goroutines
	relCache   map[string]relResult
	unanalysed []unanalysedRecord
}

type relResult struct {
	rel string
	ok  bool
}

// loadProject type-checks every package of the repository at root.
//
// Cost control (see PROFILE.md): the old helper built SSA for every dependency and kept type
// information for every function body of every dependency. This loader still reads dependencies from
// source (no compiled export data: that would need a build cache that matches the toolchain), but it
// drops the function bodies of every file outside the root before type-checking. Declarations are
// enough to resolve what the root calls.
func loadProject(root string) (*project, error) {
	absRoot, err := filepath.Abs(root)
	if err != nil {
		return nil, fmt.Errorf("resolve root: %w", err)
	}
	p := &project{
		rootDirs:   []string{absRoot},
		rootByPath: map[string]*rootPackage{},
		moduleOf:   map[string]*packages.Module{},
		relCache:   map[string]relResult{},
	}
	if resolved, err := filepath.EvalSymlinks(absRoot); err == nil && resolved != absRoot {
		p.rootDirs = append(p.rootDirs, resolved)
	}

	cfg := &packages.Config{
		Mode: packages.NeedName |
			packages.NeedFiles |
			packages.NeedCompiledGoFiles |
			packages.NeedImports |
			packages.NeedTypes |
			packages.NeedTypesInfo |
			packages.NeedSyntax |
			packages.NeedDeps |
			packages.NeedModule,
		Dir:       absRoot,
		Tests:     false,
		ParseFile: p.parseFile,
	}
	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		return nil, fmt.Errorf("packages.Load: %w", err)
	}
	p.fset = fsetOf(pkgs, cfg)

	packages.Visit(pkgs, nil, func(pkg *packages.Package) {
		if pkg.Module != nil {
			p.moduleOf[pkg.PkgPath] = pkg.Module
		}
	})

	for _, pkg := range pkgs {
		rp := &rootPackage{pkg: pkg}
		p.rootByPath[pkg.PkgPath] = rp
		p.roots = append(p.roots, rp)
	}
	sort.Slice(p.roots, func(i, j int) bool { return p.roots[i].pkg.PkgPath < p.roots[j].pkg.PkgPath })

	for _, rp := range p.roots {
		p.classify(rp)
	}
	return p, nil
}

// fsetOf returns the file set that go/packages used. All packages share one.
func fsetOf(pkgs []*packages.Package, cfg *packages.Config) *token.FileSet {
	if cfg.Fset != nil {
		return cfg.Fset
	}
	for _, pkg := range pkgs {
		if pkg.Fset != nil {
			return pkg.Fset
		}
	}
	return token.NewFileSet()
}

// parseFile is the go/packages parse hook. Files of the repository are parsed as they are. Files of
// dependencies lose their function bodies: the type checker then reads their declarations only.
func (p *project) parseFile(fset *token.FileSet, filename string, src []byte) (*ast.File, error) {
	file, err := parser.ParseFile(fset, filename, src, parser.SkipObjectResolution)
	if file == nil {
		return file, err
	}
	if _, inRoot := p.relPath(filename); inRoot {
		return file, err
	}
	for _, decl := range file.Decls {
		fn, ok := decl.(*ast.FuncDecl)
		if !ok {
			continue
		}
		if fn.Recv == nil && fn.Name.Name == "init" {
			// go/types reports a body-less init as an error. An empty body is accepted.
			fn.Body = &ast.BlockStmt{}
		} else {
			fn.Body = nil
		}
	}
	return file, err
}

// relPath returns the root-relative path of an absolute file name. Files under vendor/ are
// dependencies, not part of the repository.
func (p *project) relPath(filename string) (string, bool) {
	p.relMu.Lock()
	defer p.relMu.Unlock()
	if cached, ok := p.relCache[filename]; ok {
		return cached.rel, cached.ok
	}
	result := relResult{}
	for _, dir := range p.rootDirs {
		if strings.HasPrefix(filename, dir+string(filepath.Separator)) {
			rel := filepath.ToSlash(filename[len(dir)+1:])
			result = relResult{rel: rel, ok: !strings.HasPrefix(rel, "vendor/")}
			break
		}
	}
	p.relCache[filename] = result
	return result.rel, result.ok
}

// position converts a token position to a root-relative file, line and byte column. It reads the
// real file position first. Only when that file is outside the root (cgo output) does it use the
// //line directive, which points back to the original source.
func (p *project) position(pos token.Pos) (position, bool) {
	if !pos.IsValid() {
		return position{}, false
	}
	raw := p.fset.PositionFor(pos, false)
	if rel, ok := p.relPath(raw.Filename); ok {
		return position{File: rel, Line: raw.Line, Col: raw.Column}, true
	}
	adjusted := p.fset.PositionFor(pos, true)
	if adjusted.Filename != raw.Filename {
		if rel, ok := p.relPath(adjusted.Filename); ok {
			return position{File: rel, Line: adjusted.Line, Col: adjusted.Column}, true
		}
	}
	return position{}, false
}

// classify decides if a package is analysed, and collects its files.
func (p *project) classify(rp *rootPackage) {
	pkg := rp.pkg
	if len(pkg.Errors) > 0 {
		if allExcludedByBuildConstraints(pkg.Errors) {
			return // nothing to compile on this platform: not a failure
		}
		p.unanalysed = append(p.unanalysed, unanalysedRecord{
			Type:   "unanalysed",
			Files:  p.rootFiles(pkg),
			Reason: "package did not type-check: " + p.errorText(pkg.Errors[0]),
			Unit:   pkg.PkgPath,
		})
		return
	}
	rp.typeChecked = true
	rp.analysedRels = p.rootFiles(pkg)
	for _, file := range pkg.Syntax {
		rel, ok := p.relPath(p.fset.PositionFor(file.Package, false).Filename)
		if !ok {
			continue // cgo output in the build cache
		}
		rp.files = append(rp.files, file)
		rp.rels = append(rp.rels, rel)
	}
}

func allExcludedByBuildConstraints(errs []packages.Error) bool {
	for _, e := range errs {
		if !strings.Contains(e.Msg, "build constraints exclude all Go files") {
			return false
		}
	}
	return true
}

func (p *project) errorText(e packages.Error) string {
	msg := e.Msg
	if e.Pos != "" {
		pos := e.Pos
		for _, dir := range p.rootDirs {
			pos = strings.TrimPrefix(pos, dir+string(filepath.Separator))
		}
		msg = pos + ": " + msg
	}
	return msg
}

// rootFiles lists the root-relative, sorted, unique source files of a package (original and
// compiled).
func (p *project) rootFiles(pkg *packages.Package) []string {
	seen := map[string]bool{}
	var files []string
	for _, list := range [][]string{pkg.GoFiles, pkg.CompiledGoFiles} {
		for _, name := range list {
			if rel, ok := p.relPath(name); ok && !seen[rel] {
				seen[rel] = true
				files = append(files, rel)
			}
		}
	}
	sort.Strings(files)
	return files
}

// moduleLabel renders the module of a package as "path@version" ("" for the standard library).
func (p *project) moduleLabel(pkgPath string) string {
	m := p.moduleOf[pkgPath]
	if m == nil {
		return ""
	}
	if m.Version == "" {
		return m.Path
	}
	return m.Path + "@" + m.Version
}
