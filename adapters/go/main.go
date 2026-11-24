// aft-go-adapter type-checks a Go repository and prints contract-v1 records for aft
// (docs/adapters/CONTRACT.md): every call site with the declared functions it can reach, and five
// kinds of facts (registers, implements, writes, returns, flows).
//
// It uses the standard type checker only (go/types through x/tools go/packages). It builds no SSA and
// no call graph. The old helper did, for every dependency too, and that was its cost (PROFILE.md).
//
// Exit codes: 0 for a whole run and for a partial run (packages that did not type-check are listed
// in `unanalysed` records); 1 when the run could not produce output; 2 for a usage error.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"runtime"
	"runtime/pprof"
)

func main() {
	root := flag.String("root", "", "repository root (required)")
	cpuProfile := flag.String("cpuprofile", "", "write a CPU profile to this file")
	memProfile := flag.String("memprofile", "", "write a heap profile (alloc_space and inuse_space) to this file")
	flag.Parse()
	if *root == "" {
		fmt.Fprintln(os.Stderr, toolName+": --root is required")
		os.Exit(2)
	}

	stopCPUProfile, err := startCPUProfile(*cpuProfile)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s: %v\n", toolName, err)
		os.Exit(1)
	}
	err = run(*root, os.Stdout, func() error { return writeHeapProfile(*memProfile) })
	if profErr := stopCPUProfile(); profErr != nil && err == nil {
		err = profErr
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s: %v\n", toolName, err)
		os.Exit(1)
	}
}

// run analyses the repository at root and writes the contract output to w. beforeWrite is passed
// to analyze.
func run(root string, w io.Writer, beforeWrite func() error) error {
	out, err := analyze(root, beforeWrite)
	if err != nil {
		return err
	}
	if err := out.write(w); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%s: %d sites, %d facts, %d packages not analysed, %d interface sites left out because of generic types\n",
		toolName, len(out.sites), len(out.facts), len(out.unanalysed), out.leftOutGenericSites)
	return nil
}

// analyze type-checks the repository at root and finds all records. beforeWrite runs when
// everything is in memory: the moment of the highest memory use, which is the right time for a heap
// profile. It may be nil.
func analyze(root string, beforeWrite func() error) (*result, error) {
	proj, err := loadProject(root)
	if err != nil {
		return nil, err
	}
	if len(proj.roots) == 0 {
		return nil, fmt.Errorf("no Go packages under %s", root)
	}
	impl := buildImplIndex(proj.roots)

	out := &result{unanalysed: proj.unanalysed}
	for _, rp := range proj.roots {
		if rp.typeChecked && len(rp.analysedRels) > 0 {
			out.analysed = append(out.analysed, analysedRecord{Type: "analysed", Files: rp.analysedRels})
		}
	}
	produceSites(proj, impl, out)
	produceRegisters(proj, impl, out)
	produceImplements(proj, impl, out)
	produceWrites(proj, out)
	produceReturns(proj, out)
	produceFlows(proj, impl, out)
	sortFacts(out.facts)
	if beforeWrite != nil {
		if err := beforeWrite(); err != nil {
			return nil, err
		}
	}
	runtime.KeepAlive(proj) // the loaded packages stay in memory until here: the heap profile shows them
	return out, nil
}

// startCPUProfile starts the CPU profile when path is not empty. The returned function stops it.
func startCPUProfile(path string) (func() error, error) {
	if path == "" {
		return func() error { return nil }, nil
	}
	f, err := os.Create(path)
	if err != nil {
		return nil, fmt.Errorf("cpu profile: %w", err)
	}
	if err := pprof.StartCPUProfile(f); err != nil {
		f.Close()
		return nil, fmt.Errorf("cpu profile: %w", err)
	}
	return func() error {
		pprof.StopCPUProfile()
		if err := f.Close(); err != nil {
			return fmt.Errorf("cpu profile: %w", err)
		}
		return nil
	}, nil
}

// writeHeapProfile writes the heap profile when path is not empty. The file holds both
// `-sample_index=alloc_space` (all allocations of the run) and `inuse_space` (what is live now).
func writeHeapProfile(path string) error {
	if path == "" {
		return nil
	}
	f, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("memory profile: %w", err)
	}
	defer f.Close()
	runtime.GC() // makes the in-use numbers exact
	if err := pprof.Lookup("heap").WriteTo(f, 0); err != nil {
		return fmt.Errorf("memory profile: %w", err)
	}
	return nil
}
