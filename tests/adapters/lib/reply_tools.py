#!/usr/bin/env python3
"""Small readers for aft graph replies (one JSON object per file), used by accept_R2.sh / accept_W1.sh.

  reply_tools.py strip <reply.json>            reply without the keys our layer adds: adapters, adapter_reason, text
  reply_tools.py same <a.json> <b.json>        exit 0 when the stripped replies are equal; else print the first difference
  reply_tools.py caller-lines <reply.json>     "file:line" of every caller in a callers reply (groups, any depth)
  reply_tools.py tree <reply.json>             root children of a call_tree reply: name file line resolved resolved_by
  reply_tools.py mentions <reply.json> <text>  exit 0 when <text> appears anywhere in the reply
  reply_tools.py adapters <reply.json>         the "adapters" status list (JSON), or exit 1 when it is missing
"""
import json
import sys

OURS = ("adapters", "adapter_reason", "text")


def load(path):
    with open(path) as fh:
        return json.load(fh)


def strip(reply):
    return {k: v for k, v in reply.items() if k not in OURS and k != "id"}


def first_diff(a, b, path="$"):
    if type(a) is not type(b):
        return f"{path}: {json.dumps(a)[:200]} != {json.dumps(b)[:200]}"
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return f"{path}.{k}: only in {'first' if k in a else 'second'}"
            d = first_diff(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: list length {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_diff(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    return None if a == b else f"{path}: {json.dumps(a)[:200]} != {json.dumps(b)[:200]}"


def caller_lines(reply):
    out = []

    def walk(groups):
        for g in groups or []:
            f = g.get("file")
            for c in g.get("callers", []):
                if "line" in c and f:
                    out.append(f"{f}:{c['line']}")
                if "callers" in c and isinstance(c["callers"], list) and c["callers"] and "file" in c["callers"][0]:
                    walk(c["callers"])
    walk(reply.get("callers"))
    return out


def main():
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "strip":
        print(json.dumps(strip(load(args[0])), indent=1, sort_keys=True))
    elif cmd == "same":
        d = first_diff(strip(load(args[0])), strip(load(args[1])))
        if d:
            print(d)
            return 1
    elif cmd == "caller-lines":
        print("\n".join(caller_lines(load(args[0]))))
    elif cmd == "tree":
        for c in load(args[0]).get("children", []):
            print(c.get("name"), c.get("file"), c.get("line"), c.get("resolved"), c.get("resolved_by"))
    elif cmd == "mentions":
        return 0 if args[1] in json.dumps(load(args[0])) else 1
    elif cmd == "adapters":
        r = load(args[0])
        if "adapters" not in r:
            return 1
        print(json.dumps(r["adapters"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
