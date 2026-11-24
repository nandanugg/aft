package main

import (
	"go/ast"
	"go/types"
)

// Producer: `implements` facts. One fact per pair (type of the repository, interface of the
// repository). Go interfaces are structural: no declaration says "T implements I", so only the type
// checker knows. The old helper also emitted these facts, one per method; one per pair is enough
// because positions of both types identify the pair.
//
// subject = the declared name of the type, object = the declared name of the interface.
// label = "pointer" when only *T implements the interface (pointer receivers), "value" when T does.
// Empty interfaces are left out: every type implements them. Generic types are left out.

func produceImplements(proj *project, impl *implIndex, out *result) {
	for _, rp := range proj.roots {
		if !rp.typeChecked || rp.pkg.Types == nil {
			continue
		}
		scope := rp.pkg.Types.Scope()
		for _, name := range scope.Names() {
			tn, ok := scope.Lookup(name).(*types.TypeName)
			if !ok || tn.IsAlias() {
				continue
			}
			iface, ok := tn.Type().(*types.Named)
			if !ok || iface.TypeParams().Len() > 0 {
				continue
			}
			if _, isInterface := iface.Underlying().(*types.Interface); !isInterface {
				continue
			}
			object, ok := proj.position(tn.Pos())
			if !ok {
				continue
			}
			for _, im := range impl.typesImplementing(iface) {
				implObj := im.named.Obj()
				owner := proj.rootByPath[implObj.Pkg().Path()]
				if owner == nil || !owner.typeChecked {
					continue
				}
				subject, ok := proj.position(implObj.Pos())
				if !ok {
					continue
				}
				fact := factRecord{
					Type: "fact", Kind: "implements",
					Subject: subject, SubjectName: implObj.Name(),
					Object: object, ObjectName: tn.Name(),
					Label: "pointer",
				}
				if im.valueToo {
					fact.Label = "value"
				}
				if isMockFile(subject.File) {
					fact.Tags = []string{"mock"}
				}
				out.facts = append(out.facts, fact)
			}
		}
	}
}

// funcDisplayName names a declared function for facts: "Name" or "Recv.Name".
func (p *project) funcDisplayName(fd *ast.FuncDecl) string {
	if fd.Recv == nil || len(fd.Recv.List) == 0 {
		return fd.Name.Name
	}
	expr := fd.Recv.List[0].Type
	for {
		switch e := expr.(type) {
		case *ast.StarExpr:
			expr = e.X
			continue
		case *ast.ParenExpr:
			expr = e.X
			continue
		case *ast.IndexExpr:
			expr = e.X
			continue
		case *ast.IndexListExpr:
			expr = e.X
			continue
		case *ast.Ident:
			return e.Name + "." + fd.Name.Name
		}
		return fd.Name.Name
	}
}
