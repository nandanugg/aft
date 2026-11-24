#!/usr/bin/env python3
"""Drive one aft process in standalone NDJSON mode: configure, then each command.

Usage: oneshot.py --bin <aft> --root <project root> '<json command>' ['<json command>' ...]
Each command is a JSON object without "id" (for example '{"command":"callers","file":"/abs/f.go","symbol":"X"}').
Prints one JSON line per command reply (the raw reply object), in order. The caller sets the
isolated environment (HOME, XDG_*, AFT_STORAGE_DIR, AFT_CACHE_DIR, registry) before calling.
Exit 0 when every command got a reply, 1 otherwise. ONESHOT_TIMEOUT_S (default 180) per reply.
"""
import argparse
import json
import os
import subprocess
import sys
import threading
import time


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("commands", nargs="+")
    args = ap.parse_args()
    timeout = float(os.environ.get("ONESHOT_TIMEOUT_S", "180"))
    proc = subprocess.Popen([args.bin], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    replies, cond = {}, threading.Condition()

    def reader():
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            with cond:
                if obj.get("id") not in replies:
                    replies[obj.get("id")] = obj
                cond.notify_all()
        with cond:
            replies["__eof__"] = True
            cond.notify_all()

    threading.Thread(target=reader, daemon=True).start()

    def send_and_wait(rid, payload):
        payload = dict(payload, id=rid)
        proc.stdin.write(json.dumps(payload) + "\n")
        proc.stdin.flush()
        deadline = time.monotonic() + timeout
        with cond:
            while rid not in replies and "__eof__" not in replies:
                left = deadline - time.monotonic()
                if left <= 0:
                    break
                cond.wait(left)
            return replies.get(rid)

    ok = True
    cfg = send_and_wait("cfg", {"command": "configure", "harness": "opencode", "project_root": os.path.abspath(args.root)})
    if not cfg:
        print(json.dumps({"oneshot_error": "no configure reply"}))
        ok = False
    else:
        for n, raw in enumerate(args.commands):
            reply = send_and_wait(f"cmd{n}", json.loads(raw))
            if reply is None:
                print(json.dumps({"oneshot_error": f"no reply to {raw}"}))
                ok = False
            else:
                print(json.dumps(reply))
    try:
        proc.stdin.close()
        proc.wait(timeout=60)
    except Exception:
        proc.kill()
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
