#!/usr/bin/env python3
"""Compare Go adapter output on one fork testdata module with the restored fork expectations.

Usage: facts_compare.py <expected.json> --module <name> --output <adapter output.ndjson>
Cases: tests/adapters/fixtures/go-facts/expected.json (from the fork's go-helper tests, decision L).
A position matches on file + line; a different column is a WARN (any column inside the call maps).
Cases marked "unsure" are INFO only; cases marked "optional" give WARN instead of FAIL.
Fields of the old helper with no place in contract v1 ("via", "receiver", "methods") are not checked.
Exit 0 when no case FAILs.
"""
import argparse
import json
import sys


def at(pos, case_pos):
    return pos and case_pos and pos["file"] == case_pos["file"] and pos["line"] == case_pos["line"]


def col_note(pos, case_pos):
    return "" if pos["col"] == case_pos["col"] else f" (col {pos['col']} vs expected {case_pos['col']})"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("expected")
    ap.add_argument("--module", required=True)
    ap.add_argument("--output", required=True)
    a = ap.parse_args()
    cases = [c for c in json.load(open(a.expected))["cases"] if c.get("module") == a.module]
    recs = [json.loads(l) for l in open(a.output) if l.strip()]
    facts = [r for r in recs if r.get("type") == "fact"]
    sites = [r for r in recs if r.get("type") == "site"]
    fails = 0

    def report(case, ok, detail=""):
        nonlocal fails
        level = "PASS" if ok else ("INFO" if case.get("unsure") else "WARN" if case.get("optional") else "FAIL")
        if level == "FAIL":
            fails += 1
        print(f"{level} {case['id']} {case['kind']} {case.get('from', '')}{': ' + detail if detail else ''}")

    for c in cases:
        k = c["kind"]
        of = [f for f in facts if f["kind"] == k]
        if k in ("registers", "implements", "writes", "returns", "flows") and "count" in c:
            report(c, len(of) == c["count"], f"{len(of)} {k} facts, expected {c['count']}")
        elif k == "registers" and c.get("absent"):
            bad = [f for f in of if not f.get("object_name")]
            report(c, not bad, f"{len(bad)} registers facts without an object name")
        elif k == "registers":
            m = [f for f in of if f.get("object_name") == c["object_name"] and at(f["subject"], c["subject"]) and at(f["object"], c["object"])]
            if c.get("label_absent"):
                m = [f for f in m if not f.get("label")]
            else:
                m = [f for f in m if f.get("label") == c["label"]]
            report(c, bool(m), "" if m else f"no registers fact {c['subject_name']} -> {c['object_name']} label {c.get('label')!r}")
        elif k == "implements":
            m = [f for f in of if f.get("subject_name") == c["subject_name"] and f.get("object_name") == c["object_name"] and at(f["subject"], c["subject"]) and at(f["object"], c["object"])]
            report(c, bool(m), "" if m else f"no implements fact {c['subject_name']} -> {c['object_name']}")
        elif k == "writes":
            names = {c["subject_name"], c.get("subject_alt_name")}
            m = [f for f in of if f.get("object_name") == c["object_name"] and f.get("subject_name") in names and at(f["object"], c["object"])]
            if c.get("absent"):
                m = [f for f in of if f.get("object_name") == c["object_name"] and f.get("subject_name") == c["subject_name"]]
                report(c, not m, "an excluded write was emitted" if m else "")
            else:
                report(c, bool(m), "" if m else f"no writes fact {sorted(n for n in names if n)} -> {c['object_name']}")
        elif k == "returns":
            m = [f for f in of if f.get("subject_name") == c["subject_name"]]
            if "min_count" in c:
                report(c, len(m) >= c["min_count"], f"{len(m)} returns facts for {c['subject_name']}, need >= {c['min_count']}")
            else:
                hit = [f for f in m if any(s in (f.get("label") or "") for s in c.get("label_contains_any", []))]
                report(c, bool(hit), "" if hit else f"no returns label of {c['subject_name']} contains any of {c.get('label_contains_any')}")
        elif k == "flows":
            m = [f for f in of if f.get("subject_name") == c["subject_name"] and f.get("object_name") == c["object_name"]]
            report(c, bool(m), "" if m else f"no flows fact {c['subject_name']} -> {c['object_name']} in {c.get('function')}")
        elif k == "site_tags":
            m = [s for s in sites if at(s, c["site"])]
            ok = any(set(c["tags"]) <= set(s.get("tags", [])) for s in m)
            report(c, ok, (col_note(m[0], c["site"]) if m and ok else f"site {c['site']} tags {[s.get('tags') for s in m]}, want {c['tags']}"))
        elif k == "site" and "count" in c:
            n = sum(1 for s in sites if {"in_goroutine", "in_defer"} & set(s.get("tags", [])) and s["targets"])
            report(c, n >= c["count"], f"{n} go/defer sites with targets, expected {c['count']}")
        elif k == "site":
            m = [s for s in sites if at(s, c["site"]) and any(at(t, c["target"]) for t in s["targets"])]
            report(c, bool(m), (col_note(m[0], c["site"]) if m else f"no site {c['site']} -> {c['target']}"))
        else:
            report(dict(c, unsure="unknown case kind"), False, "case kind not handled")
    print(f"module {a.module}: {len(cases)} cases, {fails} failed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
