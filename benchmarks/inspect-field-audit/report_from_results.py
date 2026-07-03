#!/usr/bin/env python3
"""Generate a consolidated REPORT.md skeleton from field-audit results."""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

DEFAULT_WORKDIR = Path.home() / "Work" / "OSS" / "AFT_TESTS"


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def verdict_counts(samples: list[dict[str, Any]]) -> Counter[str]:
    counts: Counter[str] = Counter()
    for sample in samples:
        counts[str(sample.get("verdict", "UNCERTAIN"))] += 1
    return counts


def tp_rate(counts: Counter[str]) -> str:
    denominator = counts["TP"] + counts["FP"]
    if denominator == 0:
        return "n/a"
    return f"{counts['TP'] / denominator:.0%}"


def category_samples(verification: dict[str, Any], category: str) -> list[dict[str, Any]]:
    return [sample for sample in verification.get("samples", []) if sample.get("category") == category]


def render_table(headers: list[str], rows: list[list[Any]]) -> str:
    lines = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    for row in rows:
        lines.append("| " + " | ".join(str(cell) for cell in row) + " |")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    workdir = Path(args.workdir).expanduser().resolve()
    output = Path(args.output).expanduser().resolve()
    if output.exists() and not args.force:
        print(f"Refusing to overwrite existing report without --force: {output}", file=sys.stderr)
        return 2

    matrix = load_json(Path(args.matrix))
    sections: list[str] = []
    sections.append("# aft_inspect field-audit report")
    sections.append("")
    sections.append("Generated skeleton. Replace UNCERTAIN verdicts and add prose for every FP class before using this as the final audit report.")
    sections.append("")
    sections.append("## Executive summary")
    sections.append("")

    rows = []
    fp_forms: defaultdict[str, list[str]] = defaultdict(list)
    for spec in matrix:
        slug = spec["slug"]
        summary_path = workdir / "_results" / slug / "summary.json"
        verify_path = workdir / "_results" / slug / "verify.json"
        summary = load_json(summary_path) if summary_path.exists() else {}
        verification = load_json(verify_path) if verify_path.exists() else {"samples": []}
        counts_by_category = {}
        for category in ("dead_code", "unused_exports", "cycles", "duplicates"):
            counts = verdict_counts(category_samples(verification, category))
            counts_by_category[category] = f"TP {counts['TP']} / FP {counts['FP']} / UNC {counts['UNCERTAIN']} ({tp_rate(counts)})"
        rows.append(
            [
                slug,
                spec["category"],
                summary.get("status", "missing"),
                summary.get("tracked_file_count", "?"),
                counts_by_category["dead_code"],
                counts_by_category["unused_exports"],
                counts_by_category["cycles"],
                counts_by_category["duplicates"],
            ]
        )
        for sample in verification.get("samples", []):
            if sample.get("verdict") == "FP":
                form = sample.get("form") or "UNCLASSIFIED_FORM"
                item = sample.get("item") or {}
                fp_forms[str(form)].append(f"{slug}: {item.get('file')}:{item.get('line')} {item.get('symbol')}")

    sections.append(render_table(["repo", "category", "status", "files", "dead_code", "unused_exports", "cycles", "duplicates"], rows))
    sections.append("")
    sections.append("## MISREPORTING: false-positive / false-negative classes")
    sections.append("")
    if not fp_forms:
        sections.append("No reviewed FP classes recorded yet. Fill `verify.json` verdict/form fields, then regenerate.")
    else:
        for form, instances in sorted(fp_forms.items()):
            sections.append(f"### {form}")
            sections.append("")
            sections.append("- Instances:")
            for instance in instances:
                sections.append(f"  - {instance}")
            sections.append("")
    sections.append("")
    sections.append("## NOISE: true but low-signal findings")
    sections.append("")
    sections.append("Record generated-code, fixtures, vendored code, and mirror-tree duplicate noise here.")
    sections.append("")
    sections.append("## GAPS: unavailable or degraded categories")
    sections.append("")
    sections.append("Record unsupported-language categories, pending/building statuses, crashes, hangs, and skipped ground-truth tools here.")
    sections.append("")
    sections.append("## Per-repo appendix")
    sections.append("")
    for spec in matrix:
        slug = spec["slug"]
        summary_path = workdir / "_results" / slug / "summary.json"
        summary = load_json(summary_path) if summary_path.exists() else {}
        sections.append(f"### {slug}")
        sections.append("")
        sections.append(f"- Category: `{spec['category']}`")
        sections.append(f"- URL: {spec['url']}")
        sections.append(f"- HEAD: `{summary.get('head', 'missing')}`")
        warmup = summary.get("warmup", {})
        polls = summary.get("inspect_polls", [])
        sections.append(f"- Warmup: {warmup.get('elapsed_s', 'n/a')}s rc={warmup.get('returncode', 'n/a')}")
        sections.append(f"- Inspect polls: {polls}")
        sections.append(f"- Scanner state: `{json.dumps(summary.get('scanner_state', {}), sort_keys=True)}`")
        sections.append(f"- Counts: `{json.dumps(summary.get('inspect_counts', {}), sort_keys=True)}`")
        sections.append("")

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("\n".join(sections) + "\n", encoding="utf-8")
    print(f"Wrote {output}")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", default=os.environ.get("AFT_AUDIT_ROOT", str(DEFAULT_WORKDIR)))
    parser.add_argument("--matrix", default=str(Path(__file__).with_name("repos.json")))
    parser.add_argument("--output", default=str(DEFAULT_WORKDIR / "_results" / "REPORT.md"))
    parser.add_argument("--force", action="store_true")
    return parser.parse_args(argv)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
