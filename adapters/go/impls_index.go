package main

import (
	"go/types"
	"sort"
)

// implIndex answers one question: which methods of the repository's own types can run when an
// interface method is called. It replaces the class-hierarchy pass of the old helper, which ran over
// every function of every dependency. Only the types declared in the repository are indexed.
//
// Generic types are not indexed: a generic type has no single method set to test. The index knows
// which methods they declare, so it can tell when a list of implementers may be incomplete.
type implIndex struct {
	// types are the repository's named, non-interface, non-generic types in a fixed order.
	types []*types.Named
	// byMethod lists, per method id, the types whose pointer method set has that method.
	byMethod map[string][]implCandidate
	// generics holds, per generic type of the repository, the ids of its methods.
	generics []map[string]bool
	memo     map[implQuery]implAnswer
}

// implAnswer is the list of implementing methods for one interface method. complete is false when
// a generic type of the repository declares all the methods of the interface: it may be an
// implementer that the list does not name.
type implAnswer struct {
	methods  []*types.Func
	complete bool
}

type implCandidate struct {
	named *types.Named
	fn    *types.Func // the method as declared; for a promoted method, the embedded type's method
}

type implQuery struct {
	iface types.Type
	id    string
}

func buildImplIndex(roots []*rootPackage) *implIndex {
	ix := &implIndex{byMethod: map[string][]implCandidate{}, memo: map[implQuery]implAnswer{}}
	for _, rp := range roots {
		if !rp.typeChecked || rp.pkg.Types == nil {
			continue
		}
		scope := rp.pkg.Types.Scope()
		for _, name := range scope.Names() {
			tn, ok := scope.Lookup(name).(*types.TypeName)
			if !ok || tn.IsAlias() {
				continue
			}
			named, ok := tn.Type().(*types.Named)
			if !ok {
				continue
			}
			if named.TypeParams().Len() > 0 {
				if _, isInterface := named.Underlying().(*types.Interface); !isInterface {
					ids := map[string]bool{}
					for i := 0; i < named.NumMethods(); i++ {
						ids[named.Method(i).Id()] = true
					}
					ix.generics = append(ix.generics, ids)
				}
				continue
			}
			if _, isInterface := named.Underlying().(*types.Interface); isInterface {
				continue
			}
			ix.types = append(ix.types, named)
			methods := types.NewMethodSet(types.NewPointer(named))
			for i := 0; i < methods.Len(); i++ {
				fn, ok := methods.At(i).Obj().(*types.Func)
				if !ok {
					continue
				}
				ix.byMethod[fn.Id()] = append(ix.byMethod[fn.Id()], implCandidate{named: named, fn: fn})
			}
		}
	}
	return ix
}

// implementersOf returns the declared methods that can run for a call of method m on a value of
// the interface type iface. The result has no duplicates. The second result is false when the list
// may be incomplete because of a generic type (see implAnswer).
func (ix *implIndex) implementersOf(iface types.Type, m *types.Func) ([]*types.Func, bool) {
	query := implQuery{iface: iface, id: m.Id()}
	if cached, ok := ix.memo[query]; ok {
		return cached.methods, cached.complete
	}
	answer := implAnswer{complete: true}
	if underlying, ok := iface.Underlying().(*types.Interface); ok {
		seen := map[*types.Func]bool{}
		for _, c := range ix.byMethod[query.id] {
			if seen[c.fn] || declaredOnInterface(c.fn) || !types.Implements(types.NewPointer(c.named), underlying) {
				continue
			}
			seen[c.fn] = true
			answer.methods = append(answer.methods, c.fn)
		}
		answer.complete = !ix.genericMayImplement(underlying)
	}
	ix.memo[query] = answer
	return answer.methods, answer.complete
}

// genericMayImplement is true when some generic type of the repository declares every method of the
// interface.
func (ix *implIndex) genericMayImplement(iface *types.Interface) bool {
	for _, ids := range ix.generics {
		all := true
		for i := 0; i < iface.NumMethods(); i++ {
			if !ids[iface.Method(i).Id()] {
				all = false
				break
			}
		}
		if all {
			return true
		}
	}
	return false
}

// declaredOnInterface is true for a method that a struct promotes from an embedded interface: the
// type satisfies the interface, but there is no declared body of its own to run.
func declaredOnInterface(fn *types.Func) bool {
	recv := fn.Type().(*types.Signature).Recv()
	return recv != nil && isInterfaceType(recv.Type())
}

// implementer is a repository type that satisfies an interface. valueToo is true when the value
// type satisfies it as well, not only the pointer type.
type implementer struct {
	named    *types.Named
	valueToo bool
}

// typesImplementing returns the repository types whose pointer method set satisfies iface.
func (ix *implIndex) typesImplementing(iface *types.Named) []implementer {
	underlying, ok := iface.Underlying().(*types.Interface)
	if !ok || underlying.NumMethods() == 0 {
		return nil
	}
	// Start from the interface method with the fewest candidates.
	var pool []implCandidate
	for i := 0; i < underlying.NumMethods(); i++ {
		candidates := ix.byMethod[underlying.Method(i).Id()]
		if i == 0 || len(candidates) < len(pool) {
			pool = candidates
		}
	}
	seen := map[*types.Named]bool{}
	var result []implementer
	for _, c := range pool {
		if seen[c.named] {
			continue
		}
		seen[c.named] = true
		if types.Implements(types.NewPointer(c.named), underlying) {
			result = append(result, implementer{named: c.named, valueToo: types.Implements(c.named, underlying)})
		}
	}
	sort.Slice(result, func(i, j int) bool {
		a, b := result[i].named.Obj(), result[j].named.Obj()
		if a.Pkg().Path() != b.Pkg().Path() {
			return a.Pkg().Path() < b.Pkg().Path()
		}
		return a.Name() < b.Name()
	})
	return result
}
