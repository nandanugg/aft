#!/usr/bin/env python3
"""Build a contract-v1 fixture overlay (Go) from the Phase 1 spike helper output.

The spike helper (go-helper-spike in the build session's scratchpad, run with -all -external) prints the fork's
edge JSON plus caller col/offset (open parenthesis) and callee line/col. This
script turns it into what a contract-v1 Go adapter would print, with the
decisions of aft-contract-decisions.md applied:
  - one `site` per call; in-root targets; mock targets tagged `mock` (decision 5)
  - sites whose only callees are outside the root kept with empty targets (decision 4a)
  - no signature-match fan-out: a non-interface call with more than one target is left out (decision 6)
  - `analysed` = compiled non-test .go files (tracked by git, not under testdata/)
  - no facts, no tags, no external labels (the real Go adapter adds them; this fixture
    only feeds the call-graph side for R2)
  - `name` = the called name as written before the '(' in the source

Usage: build_fixture_overlay.py <helper.json> <repo root> <out.ndjson>
"""
import json
import os
import subprocess
import sys
from collections import OrderedDict


def written_name(line_bytes, col):
    """The identifier just before the '(' at 1-based byte column `col`."""
    i = col - 2
    # Explicit instantiation: F[int](...) -> skip the [...] part.
    if i >= 0 and line_bytes[i:i + 1] == b"]":
        depth = 0
        while i >= 0:
            ch = line_bytes[i:i + 1]
            if ch == b"]":
                depth += 1
            elif ch == b"[":
                depth -= 1
                if depth == 0:
                    i -= 1
                    break
            i -= 1
    end = i + 1
    while i >= 0 and (line_bytes[i:i + 1].isalnum() or line_bytes[i:i + 1] == b"_"):
        i -= 1
    return line_bytes[i + 1:end].decode("utf-8", "replace")


def main():
    helper_path, root, out_path = sys.argv[1:4]
    helper = json.load(open(helper_path))
    tracked = subprocess.run(["/usr/bin/git", "-C", root, "ls-files", "*.go"], capture_output=True, text=True, check=True).stdout.split()
    analysed = sorted(f for f in tracked if not f.endswith("_test.go") and "/testdata/" not in f and not f.startswith("testdata/"))
    sites = OrderedDict()
    for e in helper["edges"]:
        if e["kind"] in ("dispatches", "goroutine", "defer"):
            continue
        c = e["caller"]
        key = (c["file"], c["offset"])
        site = sites.setdefault(key, {"file": c["file"], "line": c["line"], "col": c["col"], "kinds": set(), "targets": OrderedDict()})
        site["kinds"].add(e["kind"])
        t = e["callee"]
        if t.get("external"):
            continue
        name = t["symbol"].split("[", 1)[0]
        tags = ["mock"] if ("/mocks/" in t["file"] or t["file"].startswith("mocks/")) else []
        site["targets"][(t["file"], t["line"], t["col"])] = {
            "file": t["file"], "line": t["line"], "col": t["col"], "name": name, "kind": e["kind"], "tags": tags,
        }
    lines_cache = {}
    records = []
    dropped_fanout = 0
    for (file, _), s in sorted(sites.items()):
        targets = list(s["targets"].values())
        edges = len(s["targets"])
        if "interface" not in s["kinds"] and edges > 1:
            dropped_fanout += 1
            continue
        if file not in lines_cache:
            with open(os.path.join(root, file), "rb") as fh:
                lines_cache[file] = fh.read().split(b"\n")
        name = written_name(lines_cache[file][s["line"] - 1], s["col"])
        rec = {"type": "site", "file": file, "line": s["line"], "col": s["col"], "name": name, "targets": targets}
        records.append(rec)
    header = {"type": "header", "contract": 1, "language": "go", "tool": "spike-helper-fixture", "tool_version": "fork bd760571 + Phase 1 spike"}
    end = {
        "type": "end",
        "sites": len(records),
        "targets": sum(len(r["targets"]) for r in records),
        "facts": 0,
        "analysed_files": len(analysed),
        "unanalysed_files": 0,
    }
    with open(out_path, "w") as out:
        out.write(json.dumps(header) + "\n")
        out.write(json.dumps({"type": "analysed", "files": analysed}) + "\n")
        for r in records:
            out.write(json.dumps(r, separators=(",", ":")) + "\n")
        out.write(json.dumps(end) + "\n")
    print(json.dumps({"out": out_path, "sites": end["sites"], "targets": end["targets"], "analysed_files": len(analysed), "dropped_signature_fanout_sites": dropped_fanout}))


if __name__ == "__main__":
    main()
