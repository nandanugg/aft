#!/usr/bin/env python3
"""Check adapter output against contract v1 (docs/adapters/CONTRACT.md), without Rust.

Same rules as `lang_adapters::contract::parse_output`, plus, with --root, the
file checks the runner does at ingest (every site and target position is inside
its file, every analysed file exists).

Usage:
  contract_check.py <output.ndjson> --registry <registry.json> --language <lang> [--root <repo>] [--summary <out.json>]
Exit 0 = valid. Exit 1 = violation (first one printed). Exit 2 = usage error.
"""
import argparse
import json
import os
import sys


def rel_ok(path):
    return bool(path) and not path.startswith("/") and "\\" not in path and all(p not in ("", "..") for p in path.split("/"))


class Violation(Exception):
    def __init__(self, line, message):
        super().__init__(f"contract violation at line {line}: {message}" if line else f"contract violation: {message}")


def check_pos(line, what, file, l, c, may_be_absolute=False):
    if not (rel_ok(file) or (may_be_absolute and isinstance(file, str) and file.startswith("/"))):
        raise Violation(line, f"{what} file {file!r} is not a root-relative path")
    if not isinstance(l, int) or not isinstance(c, int) or l < 1 or c < 1:
        raise Violation(line, f"{what} position {file}:{l}:{c} is not 1-based")


def parse(data, entry):
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise Violation(0, f"stdout is not UTF-8: {error}")
    if not text:
        raise Violation(0, "no output (no header and no end record)")
    if not text.endswith("\n"):
        raise Violation(len(text.split("\n")), "the last line is incomplete (no newline at the end)")
    declared = {fact["kind"] for fact in entry.get("facts", [])}
    header, end = None, None
    analysed, sites, facts, unanalysed = set(), [], [], []
    for number, raw in enumerate(text[:-1].split("\n"), start=1):
        if not raw.strip():
            raise Violation(number, "empty line")
        if end is not None:
            raise Violation(number, "a record after the end record")
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError as error:
            raise Violation(number, f"not a contract record: {error}")
        kind = rec.get("type") if isinstance(rec, dict) else None
        if header is None and kind != "header":
            raise Violation(number, "the first record must be the header")
        if kind == "header":
            if header is not None:
                raise Violation(number, "a second header")
            if rec.get("contract") != 1 or rec.get("contract") != entry["contract"]:
                raise Violation(number, f"header contract {rec.get('contract')} does not match the registry entry ({entry['contract']}) and this build (1)")
            if rec.get("language") != entry["language"]:
                raise Violation(number, f"header language {rec.get('language')!r} is not the registry language {entry['language']!r}")
            if not isinstance(rec.get("tool"), str):
                raise Violation(number, "not a contract record: header without tool")
            header = rec
        elif kind == "analysed":
            for f in rec["files"]:
                if not rel_ok(f):
                    raise Violation(number, f"analysed file {f!r} is not a root-relative path")
                analysed.add(f)
        elif kind == "site":
            for field in ("file", "line", "col", "name", "targets"):
                if field not in rec:
                    raise Violation(number, f"not a contract record: site without {field}")
            check_pos(number, "site", rec["file"], rec["line"], rec["col"])
            for t in rec["targets"]:
                for field in ("file", "line", "col", "name", "kind"):
                    if field not in t:
                        raise Violation(number, f"not a contract record: target without {field}")
                check_pos(number, "target", t["file"], t["line"], t["col"])
            sites.append(rec)
        elif kind == "fact":
            if rec.get("kind") not in declared:
                raise Violation(number, f"fact kind {rec.get('kind')!r} is not declared in the registry entry")
            s, o = rec["subject"], rec["object"]
            check_pos(number, "fact subject", s["file"], s["line"], s["col"])
            check_pos(number, "fact object", o["file"], o["line"], o["col"], may_be_absolute=True)
            facts.append(rec)
        elif kind == "unanalysed":
            for f in rec["files"]:
                if not rel_ok(f):
                    raise Violation(number, f"unanalysed file {f!r} is not a root-relative path")
            if not isinstance(rec.get("reason"), str):
                raise Violation(number, "not a contract record: unanalysed without reason")
            unanalysed.append(rec)
        elif kind == "end":
            end = rec
        else:
            raise Violation(number, f"not a contract record: unknown type {kind!r}")
    if header is None:
        raise Violation(0, "no header record")
    if end is None:
        raise Violation(0, "no end record (the adapter stopped early or crashed)")
    counted = {
        "sites": len(sites),
        "targets": sum(len(s["targets"]) for s in sites),
        "facts": len(facts),
        "analysed_files": len(analysed),
        "unanalysed_files": sum(len(u["files"]) for u in unanalysed),
    }
    if {k: end.get(k) for k in counted} != counted:
        raise Violation(0, f"end record counts {end} do not match the records read {counted}")
    return {"header": header, "analysed": analysed, "sites": sites, "facts": facts, "unanalysed": unanalysed, "end": end}


def check_files(out, root):
    cache = {}

    def lines(f):
        if f not in cache:
            with open(os.path.join(root, f), "rb") as fh:
                cache[f] = fh.read().split(b"\n")
        return cache[f]

    for f in out["analysed"]:
        if not os.path.isfile(os.path.join(root, f)):
            raise Violation(0, f"analysed file {f} does not exist under the root")
    for s in out["sites"]:
        for what, f, l, c in [("site", s["file"], s["line"], s["col"])] + [("target", t["file"], t["line"], t["col"]) for t in s["targets"]]:
            try:
                text = lines(f)
            except OSError:
                raise Violation(0, f"{what} file {f} does not exist under the root")
            if l > len(text) or c > len(text[l - 1]):
                raise Violation(0, f"{what} position {f}:{l}:{c} is not inside the file")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("output")
    ap.add_argument("--registry", required=True)
    ap.add_argument("--language", required=True)
    ap.add_argument("--root")
    ap.add_argument("--summary")
    args = ap.parse_args()
    registry = json.load(open(args.registry))
    entries = [e for e in registry["adapters"] if e["language"] == args.language]
    if len(entries) != 1:
        print(f"registry has {len(entries)} entries for {args.language}", file=sys.stderr)
        return 2
    data = open(args.output, "rb").read()
    try:
        out = parse(data, entries[0])
        if args.root:
            check_files(out, args.root)
    except Violation as v:
        print(v)
        return 1
    summary = dict(out["end"])
    summary["partial"] = bool(out["unanalysed"])
    summary["facts_by_kind"] = {}
    for f in out["facts"]:
        summary["facts_by_kind"][f["kind"]] = summary["facts_by_kind"].get(f["kind"], 0) + 1
    if args.summary:
        json.dump(summary, open(args.summary, "w"), indent=1)
    print("ok " + json.dumps(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
