#!/usr/bin/env python3
"""Restore dataset order for the unordered part files produced by the runner.

Rows that never completed (watchdog exit / crash) stay in the denominator and
are emitted with status "hang" and an empty markdown, matching the D1 rule that
every sample is scored.
"""

import argparse
import json
import statistics
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("parts", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--summary", type=Path)
    args = parser.parse_args()

    dataset = []
    with args.dataset.open(encoding="utf-8") as f:
        for line in f:
            if line.strip():
                dataset.append(json.loads(line))

    parts = {}
    with args.parts.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            record = json.loads(line)
            row = record.pop("row")
            if row in parts:
                raise SystemExit(f"duplicate row {row} in parts file")
            parts[row] = record

    unknown = sorted(set(parts) - set(range(len(dataset))))
    if unknown:
        raise SystemExit(f"{len(unknown)} part rows outside dataset: {unknown[:5]}")

    missing = [i for i in range(len(dataset)) if i not in parts]
    statuses: dict[str, int] = {}
    extraction_ms = []
    empty = 0
    with args.output.open("w", encoding="utf-8") as f:
        for i, gold in enumerate(dataset):
            record = parts.get(i)
            if record is None:
                record = {
                    "track_id": gold["track_id"],
                    "markdown": "",
                    "status": "hang",
                }
            if record.get("track_id") != gold["track_id"]:
                raise SystemExit(
                    f"row {i}: track_id mismatch "
                    f"{record.get('track_id')} != {gold['track_id']}"
                )
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
            statuses[record["status"]] = statuses.get(record["status"], 0) + 1
            if not (record.get("markdown") or "").strip():
                empty += 1
            if record.get("extract_ms") is not None:
                extraction_ms.append(record["extract_ms"])

    extraction_ms.sort()

    def pct(p: float) -> float:
        if not extraction_ms:
            return 0.0
        return extraction_ms[min(len(extraction_ms) - 1, int(len(extraction_ms) * p))]

    summary = {
        "rows": len(dataset),
        "statuses": statuses,
        "missing_rows": missing,
        "empty_predictions": empty,
        "extract_ms": {
            "n": len(extraction_ms),
            "mean": statistics.fmean(extraction_ms) if extraction_ms else 0.0,
            "p50": pct(0.50),
            "p95": pct(0.95),
            "p99": pct(0.99),
            "max": extraction_ms[-1] if extraction_ms else 0.0,
        },
    }
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    if args.summary:
        args.summary.write_text(
            json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
        )


if __name__ == "__main__":
    main()
