#!/usr/bin/env python3
"""Archive the recorded xberg 1.3.9 run (2026-11-04) as results/xberg-1.3.9/summary.json.

xberg ran twice on B1 and B4, in default mode, at 1 and 16 workers, and once in layout-model mode. Outputs are not
archived; this keeps the numbers the README table is built from. A new xberg release gets a new run; the same release
is not measured again.

    python3 -I score/collect_xberg.py [runs dir, default ~/mix-tools/runs-xberg]
"""
import json
import statistics
import sys
from pathlib import Path

HIST = Path(__file__).resolve().parents[1]


def speed(path, tool):
    s = json.loads((path / f"{tool}.summary.json").read_text())
    rows = [json.loads(line) for line in (path / tool / "results.jsonl").open()]
    ms = [r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None]
    return {
        "tput": s["items"] / s["wall_s"],
        "peak": s["max_peak_mb"],
        "p50": statistics.median(ms) if ms else None,
        "live": s["peak_live_workers"],
    }


def rounds(root, sub, workers, tool):
    return [speed(root / r / workers / sub, tool) for r in ("speed-r1", "speed-r2")]


def score(path, group, name, tool):
    d = json.loads(path.read_text())
    return d["scores"][group][name][tool]["all_zero_fail"]


def main():
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.home() / "mix-tools/runs-xberg"
    out = {"tool": "xberg", "version": "1.3.9", "date": "2026-11-04", "default": {}, "layout": {}}
    for sub in ("b1", "b4"):
        for w in ("w1", "w16"):
            out["default"][f"{sub}.{w}"] = rounds(root, sub, w, "xberg")
    for w in ("w1", "w16"):
        out["layout"][f"b1.{w}"] = [speed(root / "speed-layout" / w / "b1", "xberg-layout")]
    b1 = json.loads((root / "scores/quality-b1.json").read_text())
    b4 = json.loads((root / "scores/quality-b4.json").read_text())
    layout = json.loads((root / "scores-layout/quality-b1.json").read_text())
    out["raw_score_keys"] = {"b1": list(b1["scores"]), "b4": list(b4["scores"])}

    def first(d, tool):
        for sets in d["scores"].values():
            for ts in sets.values():
                if tool in ts:
                    return ts[tool]["all_zero_fail"]
        return None

    def holdout(d, tool):
        for sets in d["scores"].values():
            for name, ts in sets.items():
                if "holdout" in name and tool in ts:
                    return ts[tool]["all_zero_fail"]
        return None

    out["default"]["olmocr"] = first(b1, "xberg")
    out["layout"]["olmocr"] = first(layout, "xberg-layout")
    out["default"]["b4.holdout"] = holdout(b4, "xberg")
    dest = HIST / "results/xberg-1.3.9"
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "summary.json").write_text(json.dumps(out, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(out)[:1500])


if __name__ == "__main__":
    main()
