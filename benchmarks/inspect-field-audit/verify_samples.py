#!/usr/bin/env python3
"""Create verification samples and candidate-reference evidence for inspect output.

This helper does not decide truth automatically. It records enough local evidence
for a human auditor to classify each sample as TP, FP, or UNCERTAIN without losing
which search forms were checked.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import sys
from pathlib import Path
from typing import Any

DEFAULT_WORKDIR = Path.home() / "Work" / "OSS" / "AFT_TESTS"
SAMPLE_TARGETS = {"dead_code": 15, "unused_exports": 10, "cycles": 3, "duplicates": 5}
GENERATED_HINTS = ("generated", "gen/", "__generated__", "protobuf", "proto/", "openapi", "swagger")


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def run_git_grep(repo: Path, needle: str, limit: int = 80) -> list[dict[str, Any]]:
    if not needle or len(needle) < 2:
        return []
    completed = subprocess.run(
        ["git", "grep", "-n", "--fixed-strings", "--", needle],
        cwd=repo,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
    )
    if completed.returncode not in (0, 1):
        return [{"error": completed.stderr.strip()}]
    rows = []
    for raw in completed.stdout.splitlines()[:limit]:
        file, line, text = split_grep(raw)
        rows.append({"file": file, "line": line, "text": text[:300]})
    return rows


def split_grep(raw: str) -> tuple[str, int | None, str]:
    parts = raw.split(":", 2)
    if len(parts) != 3:
        return raw, None, ""
    try:
        line = int(parts[1])
    except ValueError:
        line = None
    return parts[0], line, parts[2]


def sample_items(items: list[dict[str, Any]], target: int) -> list[dict[str, Any]]:
    if len(items) <= target:
        return items
    selected: list[dict[str, Any]] = []
    selected.extend(items[: max(1, target // 2)])
    by_ext: dict[str, dict[str, Any]] = {}
    for item in items:
        file = str(item.get("file", ""))
        ext = Path(file).suffix or "<none>"
        by_ext.setdefault(ext, item)
    for item in by_ext.values():
        if item not in selected and len(selected) < target:
            selected.append(item)
    remaining = [item for item in items if item not in selected]
    random.shuffle(remaining)
    selected.extend(remaining[: max(0, target - len(selected))])
    return selected[:target]


def candidate_needles(item: dict[str, Any]) -> list[str]:
    symbol = str(item.get("symbol") or item.get("name") or "")
    needles = [symbol]
    if symbol:
        needles.extend([f"{symbol}(", f".{symbol}", f"::{symbol}", f"{symbol}:", f"{symbol} ="])
    return dedupe([needle for needle in needles if needle])


def dedupe(values: list[str]) -> list[str]:
    seen = set()
    out = []
    for value in values:
        if value not in seen:
            seen.add(value)
            out.append(value)
    return out


def generated_noise(path: str) -> bool:
    lowered = path.lower().replace("\\", "/")
    return any(hint in lowered for hint in GENERATED_HINTS)


def verify_symbol_category(repo: Path, category: str, items: list[dict[str, Any]], target: int) -> list[dict[str, Any]]:
    samples = []
    for item in sample_items(items, target):
        refs = []
        origin_file = str(item.get("file", ""))
        for needle in candidate_needles(item):
            matches = [match for match in run_git_grep(repo, needle) if match.get("file") != origin_file]
            refs.append({"needle": needle, "matches": matches})
        samples.append(
            {
                "category": category,
                "item": item,
                "generated_or_codegen_path": generated_noise(origin_file),
                "candidate_refs": refs,
                "verdict": "UNCERTAIN",
                "missed_reference": None,
                "form": None,
                "notes": "Set verdict to TP, FP, or UNCERTAIN after inspecting candidate_refs and non-text dispatch/config paths.",
            }
        )
    return samples


def verify_cycles(cycles: list[dict[str, Any]], target: int) -> list[dict[str, Any]]:
    samples = []
    for cycle in cycles[:target]:
        samples.append(
            {
                "category": "cycles",
                "item": cycle,
                "verdict": "UNCERTAIN",
                "notes": "Follow every import edge by hand; set TP if the chain is a real cycle.",
            }
        )
    return samples


def verify_duplicates(groups: list[dict[str, Any]], target: int) -> list[dict[str, Any]]:
    samples = []
    for group in groups[:target]:
        files = [str(occ.get("file") or occ.get("path") or "") for occ in group.get("occurrences", []) if isinstance(occ, dict)]
        samples.append(
            {
                "category": "duplicates",
                "item": group,
                "generated_or_codegen_path": any(generated_noise(file) for file in files),
                "verdict": "UNCERTAIN",
                "notes": "Eyeball clone quality, line accounting, and whether this is true-but-useless generated noise.",
            }
        )
    return samples


def build_verification(repo: Path, inspect_response: dict[str, Any]) -> dict[str, Any]:
    random.seed(0)
    details = inspect_response.get("details") or {}
    samples: list[dict[str, Any]] = []
    for category in ("dead_code", "unused_exports"):
        items = details.get(category) or []
        if isinstance(items, list):
            samples.extend(verify_symbol_category(repo, category, items, SAMPLE_TARGETS[category]))
    cycles = details.get("cycles") or []
    if isinstance(cycles, list):
        samples.extend(verify_cycles(cycles, SAMPLE_TARGETS["cycles"]))
    duplicates = details.get("duplicates") or []
    if isinstance(duplicates, list):
        samples.extend(verify_duplicates(duplicates, SAMPLE_TARGETS["duplicates"]))

    test_only = []
    for category in ("dead_code_test_only", "unused_exports_test_only"):
        items = details.get(category) or []
        if isinstance(items, list):
            for item in items[:5]:
                test_only.append(
                    {
                        "category": category,
                        "item": item,
                        "verdict": "UNCERTAIN",
                        "notes": "Confirm every used_by path is test/test-support only.",
                    }
                )

    return {
        "repo": str(repo),
        "samples": samples,
        "test_only_samples": test_only,
        "instructions": [
            "Do not mark TP just because candidate_refs is empty; also inspect string dispatch, macro bodies, build scripts, and config references.",
            "For every FP, fill missed_reference as file:line and form as the exact code shape that fooled AFT.",
        ],
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", default=os.environ.get("AFT_AUDIT_ROOT", str(DEFAULT_WORKDIR)))
    parser.add_argument("--repo", required=True, help="Repo slug under $AFT_AUDIT_ROOT/repos")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    workdir = Path(args.workdir).expanduser().resolve()
    repo = workdir / "repos" / args.repo
    inspect_path = workdir / "_results" / args.repo / "inspect.json"
    if not repo.exists():
        print(f"Repo clone not found: {repo}", file=sys.stderr)
        return 2
    if not inspect_path.exists():
        print(f"Inspect result not found: {inspect_path}", file=sys.stderr)
        return 2
    verification = build_verification(repo, load_json(inspect_path))
    write_json(workdir / "_results" / args.repo / "verify.json", verification)
    print(f"Wrote {workdir / '_results' / args.repo / 'verify.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
