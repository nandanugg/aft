package main

import (
	"bufio"
	"encoding/json"
	"io"
)

// Contract v1 records (docs/adapters/CONTRACT.md section 5). Field order is the order in the contract.

const toolName = "aft-go-adapter"
const toolVersion = "1.0.0"

type position struct {
	File string `json:"file"`
	Line int    `json:"line"`
	Col  int    `json:"col"`
}

type headerRecord struct {
	Type        string `json:"type"`
	Contract    int    `json:"contract"`
	Language    string `json:"language"`
	Tool        string `json:"tool"`
	ToolVersion string `json:"tool_version"`
}

type analysedRecord struct {
	Type  string   `json:"type"`
	Files []string `json:"files"`
}

type target struct {
	File string   `json:"file"`
	Line int      `json:"line"`
	Col  int      `json:"col"`
	Name string   `json:"name"`
	Kind string   `json:"kind"`
	Tags []string `json:"tags,omitempty"`
}

type externalLabel struct {
	Name    string `json:"name"`
	Package string `json:"package"`
	Module  string `json:"module,omitempty"`
}

type siteRecord struct {
	Type     string          `json:"type"`
	File     string          `json:"file"`
	Line     int             `json:"line"`
	Col      int             `json:"col"`
	Name     string          `json:"name"`
	Tags     []string        `json:"tags,omitempty"`
	Targets  []target        `json:"targets"`
	External []externalLabel `json:"external,omitempty"`
}

type factRecord struct {
	Type        string   `json:"type"`
	Kind        string   `json:"kind"`
	Subject     position `json:"subject"`
	SubjectName string   `json:"subject_name"`
	Object      position `json:"object"`
	ObjectName  string   `json:"object_name,omitempty"`
	Label       string   `json:"label,omitempty"`
	// Via is the registration API of a `registers` fact, for example `(*asynq.ServeMux).HandleFunc`.
	Via  string   `json:"via,omitempty"`
	Tags []string `json:"tags,omitempty"`
}

type unanalysedRecord struct {
	Type   string   `json:"type"`
	Files  []string `json:"files"`
	Reason string   `json:"reason"`
	Unit   string   `json:"unit"`
}

type endRecord struct {
	Type            string `json:"type"`
	Sites           int    `json:"sites"`
	Targets         int    `json:"targets"`
	Facts           int    `json:"facts"`
	AnalysedFiles   int    `json:"analysed_files"`
	UnanalysedFiles int    `json:"unanalysed_files"`
}

// result is everything one run found. write turns it into NDJSON; the end record counts
// exactly what write printed.
type result struct {
	analysed   []analysedRecord
	sites      []siteRecord
	facts      []factRecord
	unanalysed []unanalysedRecord
	// leftOutGenericSites counts interface calls that got no site record because a generic type
	// may implement the interface (see implAnswer). It is not part of the contract output; main
	// prints it on stderr.
	leftOutGenericSites int
}

func (r *result) write(w io.Writer) error {
	bw := bufio.NewWriterSize(w, 1<<20)
	enc := json.NewEncoder(bw)
	enc.SetEscapeHTML(false)

	end := endRecord{Type: "end"}
	analysedFiles := map[string]bool{}

	records := []any{headerRecord{Type: "header", Contract: 1, Language: "go", Tool: toolName, ToolVersion: toolVersion}}
	for _, rec := range r.analysed {
		records = append(records, rec)
		for _, f := range rec.Files {
			analysedFiles[f] = true
		}
	}
	for _, rec := range r.unanalysed {
		records = append(records, rec)
		end.UnanalysedFiles += len(rec.Files)
	}
	for _, rec := range records {
		if err := enc.Encode(rec); err != nil {
			return err
		}
	}
	for i := range r.sites {
		if err := enc.Encode(&r.sites[i]); err != nil {
			return err
		}
		end.Sites++
		end.Targets += len(r.sites[i].Targets)
	}
	for i := range r.facts {
		if err := enc.Encode(&r.facts[i]); err != nil {
			return err
		}
		end.Facts++
	}
	end.AnalysedFiles = len(analysedFiles)
	if err := enc.Encode(end); err != nil {
		return err
	}
	return bw.Flush()
}
