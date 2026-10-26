#!/usr/bin/env python3
"""PageSift 0.50 Domain 4 (DOM Relocation & Deep Self-Healing) Master Benchmark Runner.

Evaluates 4 competing engines across 32 cores under a 120 GB RAM Watchdog:
1. PageSift 0.50 (Pure Rust ElementFingerprint + FuzzyAnchor, 32 native threads)
2. Scrapling 0.5 (Python Adaptor.relocate, 32 worker processes)
3. Selectolax (Lexbor) + RapidFuzz (C/Python, 32 worker processes)
4. BeautifulSoup4 + lxml + RapidFuzz (Python, 32 worker processes)

Evaluates:
- Tier 1: 2,000 Complex Mutation Testcases across 8 Categories
- Tier 2: Ultra-Extreme Insane Chaos Stress Tests:
  - Pathological DOM Bomb (Depths 200, 500, 1000, 2000)
  - 10,000 Sibling Decoy Fanout Bomb
  - 5,000 Continuous Chaos Stream (RSS flatline leak test)
"""

from __future__ import annotations

import json
import multiprocessing as mp
import os
import subprocess
import sys
import threading
import time
from collections import defaultdict
from pathlib import Path
from typing import Any, Dict, List, Tuple

import psutil
from rapidfuzz import fuzz

try:
    from scrapling.parser import Adaptor
    HAS_SCRAPLING = True
except ImportError:
    HAS_SCRAPLING = False

try:
    from selectolax.lexbor import LexborHTMLParser
    HAS_SELECTOLAX = True
except ImportError:
    HAS_SELECTOLAX = False

try:
    from bs4 import BeautifulSoup
    HAS_BS4 = True
except ImportError:
    HAS_BS4 = False

DATA_DIR = Path("benchmarks/data/d4")
RESULTS_DIR = Path("benchmarks/history/0.50/results")
RESULTS_DIR.mkdir(parents=True, exist_ok=True)

TIER1_FILE = DATA_DIR / "tier1_mutations_2000.jsonl"
TIER2_NESTING_FILE = DATA_DIR / "tier2_pathological_nesting.jsonl"
TIER2_FANOUT_FILE = DATA_DIR / "tier2_adversarial_fanout_10k.jsonl"
TIER2_STREAM_FILE = DATA_DIR / "tier2_chaos_stream_5000.jsonl"

PS_BIN = Path("target/release/examples/bench_relocation_mt")

NUM_WORKERS = 32
MEMORY_LIMIT_GB = 120.0


def norm_text(s: str) -> str:
    if s is None:
        return ""
    return "".join(s.split()).lower()


# ---------------------------------------------------------------------------
# Watchdog & RSS Tracking
# ---------------------------------------------------------------------------
class MemoryWatchdog:
    def __init__(self, limit_gb: float = 120.0, poll_interval: float = 0.05):
        self.limit_bytes = limit_gb * 1024 * 1024 * 1024
        self.poll_interval = poll_interval
        self.peak_rss_bytes = 0
        self.stop_event = threading.Event()
        self.thread: threading.Thread | None = None
        self.aborted = False

    def _monitor(self, target_pids: list[int]):
        while not self.stop_event.is_set():
            total_rss = 0
            for pid in target_pids:
                try:
                    p = psutil.Process(pid)
                    total_rss += p.memory_info().rss
                    for child in p.children(recursive=True):
                        try:
                            total_rss += child.memory_info().rss
                        except (psutil.NoSuchProcess, psutil.AccessDenied):
                            pass
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
            if total_rss > self.peak_rss_bytes:
                self.peak_rss_bytes = total_rss
            if total_rss > self.limit_bytes:
                self.aborted = True
                eprintln(f"[WATCHDOG ALERT] Memory exceeded {self.limit_bytes / 1e9:.1f} GB! Aborting.")
                break
            time.sleep(self.poll_interval)

    def start(self, target_pids: list[int] | None = None):
        if target_pids is None:
            target_pids = [os.getpid()]
        self.stop_event.clear()
        self.peak_rss_bytes = 0
        self.aborted = False
        self.thread = threading.Thread(target=self._monitor, args=(target_pids,), daemon=True)
        self.thread.start()

    def stop(self) -> float:
        self.stop_event.set()
        if self.thread:
            self.thread.join(timeout=1.0)
        return self.peak_rss_bytes / (1024 * 1024)  # MB


# ---------------------------------------------------------------------------
# Python Worker Implementations (Picklable Top-Level Functions)
# ---------------------------------------------------------------------------
def _eval_scrapling_case(c: dict) -> dict:
    t0 = time.perf_counter()
    try:
        p1 = Adaptor(c["original_html"])
        target = p1.css(c["selector"])
        if not target:
            return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
        p2 = Adaptor(c["mutated_html"])
        reloc = p2.relocate(target[0])
        if not reloc:
            return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
        el = reloc[0]
        text = el.text_content().strip() if hasattr(el, "text_content") else getattr(el, "text", "").strip()
        ok = norm_text(c["expected"]) in norm_text(text)
        return {"id": c["id"], "found": True, "ok": ok, "false_pos": not ok, "text": text, "us": (time.perf_counter() - t0) * 1e6}
    except Exception as e:
        return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": str(e), "us": (time.perf_counter() - t0) * 1e6}


def _eval_selectolax_case(c: dict) -> dict:
    t0 = time.perf_counter()
    try:
        p1 = LexborHTMLParser(c["original_html"])
        target = p1.css_first(c["selector"])
        if not target:
            return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
        target_text = target.text(strip=True)
        target_tag = target.tag

        p2 = LexborHTMLParser(c["mutated_html"])
        best_node = None
        best_score = 0.0

        candidates = p2.css(target_tag)
        if not candidates:
            # Fallback to all elements if tag was mutated
            candidates = p2.root.iter()

        for node in candidates:
            cand_text = node.text(strip=True)
            if not cand_text:
                continue
            s = fuzz.token_set_ratio(target_text, cand_text)
            if s > best_score:
                best_score = s
                best_node = node

        if best_node and best_score >= 50.0:
            found_text = best_node.text(strip=True)
            ok = norm_text(c["expected"]) in norm_text(found_text)
            return {"id": c["id"], "found": True, "ok": ok, "false_pos": not ok, "text": found_text, "us": (time.perf_counter() - t0) * 1e6}
        return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
    except Exception as e:
        return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": str(e), "us": (time.perf_counter() - t0) * 1e6}


def _eval_bs4_case(c: dict) -> dict:
    t0 = time.perf_counter()
    try:
        s1 = BeautifulSoup(c["original_html"], "lxml")
        target = s1.select_one(c["selector"])
        if not target:
            return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
        target_text = target.get_text(strip=True)
        target_tag = target.name

        s2 = BeautifulSoup(c["mutated_html"], "lxml")
        candidates = s2.find_all(target_tag)
        if not candidates:
            candidates = s2.find_all()

        best_node = None
        best_score = 0.0
        for node in candidates:
            cand_text = node.get_text(strip=True)
            if not cand_text:
                continue
            s = fuzz.token_set_ratio(target_text, cand_text)
            if s > best_score:
                best_score = s
                best_node = node

        if best_node and best_score >= 50.0:
            found_text = best_node.get_text(strip=True)
            ok = norm_text(c["expected"]) in norm_text(found_text)
            return {"id": c["id"], "found": True, "ok": ok, "false_pos": not ok, "text": found_text, "us": (time.perf_counter() - t0) * 1e6}
        return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": "", "us": (time.perf_counter() - t0) * 1e6}
    except Exception as e:
        return {"id": c["id"], "found": False, "ok": False, "false_pos": False, "text": str(e), "us": (time.perf_counter() - t0) * 1e6}


# ---------------------------------------------------------------------------
# Runner Functions
# ---------------------------------------------------------------------------
def run_python_suite(cases: list[dict], engine_name: str, fn) -> Tuple[list[dict], float, float]:
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    watchdog.start([os.getpid()])
    t_start = time.perf_counter()

    ctx = mp.get_context("fork") if "fork" in mp.get_all_start_methods() else mp.get_context()
    with ctx.Pool(NUM_WORKERS) as pool:
        results = pool.map(fn, cases)

    wall_s = time.perf_counter() - t_start
    peak_mb = watchdog.stop()
    return results, wall_s, peak_mb


def run_pagesift_tier1(cases: list[dict]) -> Tuple[list[dict], float, float]:
    tmp_in = Path("/tmp/ps_d4_in.jsonl")
    tmp_out = Path("/tmp/ps_d4_out.tsv")
    with open(tmp_in, "w", encoding="utf-8") as f:
        for c in cases:
            f.write(json.dumps(c, ensure_ascii=False) + "\n")

    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    t_start = time.perf_counter()

    proc = subprocess.Popen([str(PS_BIN), str(tmp_in), str(NUM_WORKERS), str(tmp_out)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t_start
    peak_mb = watchdog.stop()

    if proc.returncode != 0:
        raise RuntimeError(f"PageSift exited with code {proc.returncode}: {stderr.decode()}")

    case_map = {c["id"]: c for c in cases}
    results = []
    with open(tmp_out, "r", encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            parts = line.strip().split("\t")
            if len(parts) >= 5:
                cid, found, text, score, us = parts[0], parts[1] == "true", parts[2], float(parts[3]), float(parts[4])
                c = case_map.get(cid, {})
                exp = c.get("expected", "")
                ok = found and norm_text(exp) in norm_text(text)
                false_pos = found and not ok
                results.append({
                    "id": cid,
                    "category": c.get("category", "Unknown"),
                    "found": found,
                    "ok": ok,
                    "false_pos": false_pos,
                    "text": text,
                    "us": us,
                })
    return results, wall_s, peak_mb


def compute_metrics(results: list[dict], wall_s: float, peak_mb: float) -> dict:
    total = len(results)
    corr = sum(1 for r in results if r["ok"])
    false_pos = sum(1 for r in results if r["false_pos"])
    not_found = total - corr - false_pos

    latencies = [r["us"] for r in results]
    latencies.sort()

    mean_us = sum(latencies) / total if total > 0 else 0.0
    p50_us = latencies[int(total * 0.50)] if total > 0 else 0.0
    p95_us = latencies[int(total * 0.95)] if total > 0 else 0.0
    p99_us = latencies[int(total * 0.99)] if total > 0 else 0.0

    return {
        "total": total,
        "correct": corr,
        "recall_pct": (corr / total * 100.0) if total > 0 else 0.0,
        "false_positive": false_pos,
        "false_positive_pct": (false_pos / total * 100.0) if total > 0 else 0.0,
        "not_found": not_found,
        "not_found_pct": (not_found / total * 100.0) if total > 0 else 0.0,
        "wall_time_s": wall_s,
        "throughput_ops_s": total / wall_s if wall_s > 0 else 0.0,
        "mean_latency_us": mean_us,
        "p50_latency_us": p50_us,
        "p95_latency_us": p95_us,
        "p99_latency_us": p99_us,
        "peak_rss_mb": peak_mb,
    }


def compute_category_breakdown(cases: list[dict], results: list[dict]) -> dict:
    case_map = {c["id"]: c for c in cases}
    cat_stats = defaultdict(lambda: {"total": 0, "correct": 0, "false_pos": 0})
    for r in results:
        cid = r["id"]
        cat = case_map.get(cid, {}).get("category", r.get("category", "Unknown"))
        cat_stats[cat]["total"] += 1
        if r["ok"]:
            cat_stats[cat]["correct"] += 1
        elif r["false_pos"]:
            cat_stats[cat]["false_pos"] += 1
    return dict(cat_stats)


# ---------------------------------------------------------------------------
# Tier 2: Pathological Stress Test Runner
# ---------------------------------------------------------------------------
def run_tier2_stress():
    print("\n" + "=" * 80)
    print("=== TIER 2: ULTRA-EXTREME INSANE CHAOS STRESS TESTS ===")
    print("=" * 80)

    with open(TIER2_NESTING_FILE) as f:
        nesting_cases = [json.loads(line) for line in f]

    with open(TIER2_FANOUT_FILE) as f:
        fanout_cases = [json.loads(line) for line in f]

    tier2_results = {}

    # Test 1: Pathological DOM Bomb (Depths 200, 500, 1000, 2000)
    print("\n--- [Tier 2.1] Pathological DOM Bomb (Depths: 200, 500, 1,000, 2,000) ---")
    nesting_summary = []
    for c in nesting_cases:
        depth = c["depth"]
        # PageSift
        ps_res, ps_s, ps_mb = run_pagesift_tier1([c])
        ps_ok = ps_res[0]["ok"] if ps_res else False
        ps_us = ps_res[0]["us"] if ps_res else 0.0

        # Scrapling
        sc_res = _eval_scrapling_case(c)
        sc_ok = sc_res["ok"]
        sc_us = sc_res["us"]

        # Selectolax
        sel_res = _eval_selectolax_case(c)
        sel_ok = sel_res["ok"]
        sel_us = sel_res["us"]

        # BS4
        bs4_res = _eval_bs4_case(c)
        bs4_ok = bs4_res["ok"]
        bs4_us = bs4_res["us"]

        print(f"Depth {depth:4d} | PS: {'OK' if ps_ok else 'FAIL'} ({ps_us:8.1f}µs) | Scrapling: {'OK' if sc_ok else 'FAIL'} ({sc_us:8.1f}µs) | Lexbor: {'OK' if sel_ok else 'FAIL'} ({sel_us:8.1f}µs) | BS4: {'OK' if bs4_ok else 'FAIL'} ({bs4_us:8.1f}µs)")
        nesting_summary.append({
            "depth": depth,
            "pagesift": {"ok": ps_ok, "latency_us": ps_us},
            "scrapling": {"ok": sc_ok, "latency_us": sc_us},
            "selectolax": {"ok": sel_ok, "latency_us": sel_us},
            "bs4": {"ok": bs4_ok, "latency_us": bs4_us},
        })
    tier2_results["pathological_nesting"] = nesting_summary

    # Test 2: Adversarial Sibling Fanout Bomb (10,000 elements)
    print("\n--- [Tier 2.2] 10,000 Sibling Decoy Fanout Bomb ---")
    fan_case = fanout_cases[0]
    ps_fan, ps_fan_s, ps_fan_mb = run_pagesift_tier1([fan_case])
    ps_fan_res = ps_fan[0]

    sc_fan_t0 = time.perf_counter()
    sc_fan_res = _eval_scrapling_case(fan_case)
    sc_fan_s = time.perf_counter() - sc_fan_t0

    sel_fan_t0 = time.perf_counter()
    sel_fan_res = _eval_selectolax_case(fan_case)
    sel_fan_s = time.perf_counter() - sel_fan_t0

    bs4_fan_t0 = time.perf_counter()
    bs4_fan_res = _eval_bs4_case(fan_case)
    bs4_fan_s = time.perf_counter() - bs4_fan_t0

    print(f"PageSift:   OK={ps_fan_res['ok']}, FalsePos={ps_fan_res['false_pos']}, Time={ps_fan_res['us']/1000:.2f}ms")
    print(f"Scrapling:  OK={sc_fan_res['ok']}, FalsePos={sc_fan_res['false_pos']}, Time={sc_fan_res['us']/1000:.2f}ms")
    print(f"Selectolax: OK={sel_fan_res['ok']}, FalsePos={sel_fan_res['false_pos']}, Time={sel_fan_res['us']/1000:.2f}ms")
    print(f"BS4:        OK={bs4_fan_res['ok']}, FalsePos={bs4_fan_res['false_pos']}, Time={bs4_fan_res['us']/1000:.2f}ms")

    tier2_results["fanout_10k"] = {
        "pagesift": {"ok": ps_fan_res["ok"], "false_pos": ps_fan_res["false_pos"], "ms": ps_fan_res["us"] / 1000.0},
        "scrapling": {"ok": sc_fan_res["ok"], "false_pos": sc_fan_res["false_pos"], "ms": sc_fan_res["us"] / 1000.0},
        "selectolax": {"ok": sel_fan_res["ok"], "false_pos": sel_fan_res["false_pos"], "ms": sel_fan_res["us"] / 1000.0},
        "bs4": {"ok": bs4_fan_res["ok"], "false_pos": bs4_fan_res["false_pos"], "ms": bs4_fan_res["us"] / 1000.0},
    }

    # Test 3: Continuous Chaos Stream (5,000 cases)
    print("\n--- [Tier 2.3] Continuous Chaos Stream (5,000 Sequential Mutations) ---")
    with open(TIER2_STREAM_FILE) as f:
        stream_cases = [json.loads(line) for line in f]

    ps_str_res, ps_str_s, ps_str_mb = run_pagesift_tier1(stream_cases)
    ps_str_metrics = compute_metrics(ps_str_res, ps_str_s, ps_str_mb)
    print(f"PageSift Stream: {ps_str_metrics['correct']}/{len(stream_cases)} ({ps_str_metrics['recall_pct']:.2f}%) in {ps_str_s:.3f}s ({ps_str_metrics['throughput_ops_s']:.1f} ops/s), Peak RSS: {ps_str_mb:.1f} MB")

    tier2_results["chaos_stream_5000"] = ps_str_metrics
    return tier2_results


# ---------------------------------------------------------------------------
# Main Orchestration
# ---------------------------------------------------------------------------
def main():
    print("=" * 80)
    print("PageSift 0.50 Benchmark 4: DOM Element Self-Healing & Mutation Relocation")
    print(f"Platform: AMD EPYC 32 Physical Cores, Concurrency: {NUM_WORKERS}, RAM Limit: {MEMORY_LIMIT_GB} GB")
    print("=" * 80)

    if not PS_BIN.exists():
        sys.exit(f"Binary {PS_BIN} not found! Compile with: cargo build --release -p pagesift-extract --example bench_relocation_mt")

    with open(TIER1_FILE) as f:
        tier1_cases = [json.loads(line) for line in f]
    print(f"Loaded {len(tier1_cases)} Tier 1 mutation cases.")

    tier1_report = {}

    # 1. PageSift
    print("\n[1/4] Running PageSift 0.50 (Pure Rust, 32 native threads)...")
    ps_res, ps_s, ps_mb = run_pagesift_tier1(tier1_cases)
    ps_metrics = compute_metrics(ps_res, ps_s, ps_mb)
    ps_cats = compute_category_breakdown(tier1_cases, ps_res)
    tier1_report["PageSift 0.50"] = {"metrics": ps_metrics, "categories": ps_cats}
    print(f"  -> Recall: {ps_metrics['recall_pct']:.2f}%, FalsePos: {ps_metrics['false_positive_pct']:.2f}%, Latency: {ps_metrics['mean_latency_us']:.1f}µs, Wall: {ps_s:.3f}s, RSS: {ps_mb:.1f}MB")

    # 2. Scrapling
    if HAS_SCRAPLING:
        print("\n[2/4] Running Scrapling 0.5 (Python Adaptor.relocate, 32 worker processes)...")
        sc_res, sc_s, sc_mb = run_python_suite(tier1_cases, "Scrapling", _eval_scrapling_case)
        sc_metrics = compute_metrics(sc_res, sc_s, sc_mb)
        sc_cats = compute_category_breakdown(tier1_cases, sc_res)
        tier1_report["Scrapling 0.5"] = {"metrics": sc_metrics, "categories": sc_cats}
        print(f"  -> Recall: {sc_metrics['recall_pct']:.2f}%, FalsePos: {sc_metrics['false_positive_pct']:.2f}%, Latency: {sc_metrics['mean_latency_us']:.1f}µs, Wall: {sc_s:.3f}s, RSS: {sc_mb:.1f}MB")
    else:
        print("\n[2/4] Skipping Scrapling (not installed)")

    # 3. Selectolax (Lexbor) + RapidFuzz
    if HAS_SELECTOLAX:
        print("\n[3/4] Running Selectolax + RapidFuzz (C/Python, 32 worker processes)...")
        sel_res, sel_s, sel_mb = run_python_suite(tier1_cases, "Selectolax", _eval_selectolax_case)
        sel_metrics = compute_metrics(sel_res, sel_s, sel_mb)
        sel_cats = compute_category_breakdown(tier1_cases, sel_res)
        tier1_report["Selectolax + RapidFuzz"] = {"metrics": sel_metrics, "categories": sel_cats}
        print(f"  -> Recall: {sel_metrics['recall_pct']:.2f}%, FalsePos: {sel_metrics['false_positive_pct']:.2f}%, Latency: {sel_metrics['mean_latency_us']:.1f}µs, Wall: {sel_s:.3f}s, RSS: {sel_mb:.1f}MB")
    else:
        print("\n[3/4] Skipping Selectolax (not installed)")

    # 4. BeautifulSoup4 + lxml + RapidFuzz
    if HAS_BS4:
        print("\n[4/4] Running BeautifulSoup4 + lxml + RapidFuzz (Python, 32 worker processes)...")
        bs4_res, bs4_s, bs4_mb = run_python_suite(tier1_cases, "BS4", _eval_bs4_case)
        bs4_metrics = compute_metrics(bs4_res, bs4_s, bs4_mb)
        bs4_cats = compute_category_breakdown(tier1_cases, bs4_res)
        tier1_report["BS4 + lxml + RapidFuzz"] = {"metrics": bs4_metrics, "categories": bs4_cats}
        print(f"  -> Recall: {bs4_metrics['recall_pct']:.2f}%, FalsePos: {bs4_metrics['false_positive_pct']:.2f}%, Latency: {bs4_metrics['mean_latency_us']:.1f}µs, Wall: {bs4_s:.3f}s, RSS: {bs4_mb:.1f}MB")
    else:
        print("\n[4/4] Skipping BS4 (not installed)")

    # Save Tier 1 Raw JSONL
    out_tier1_jsonl = RESULTS_DIR / "benchmark4_tier1_standard_2000.jsonl"
    with open(out_tier1_jsonl, "w", encoding="utf-8") as f:
        for engine, data in tier1_report.items():
            f.write(json.dumps({"engine": engine, **data}, ensure_ascii=False) + "\n")
    print(f"\n[Tier 1] Saved raw JSONL to {out_tier1_jsonl}")

    # Run Tier 2 Stress
    tier2_results = run_tier2_stress()
    out_tier2_jsonl = RESULTS_DIR / "benchmark4_tier2_chaos_stress.jsonl"
    with open(out_tier2_jsonl, "w", encoding="utf-8") as f:
        f.write(json.dumps(tier2_results, ensure_ascii=False) + "\n")
    print(f"\n[Tier 2] Saved raw JSONL to {out_tier2_jsonl}")

    print("\n" + "=" * 80)
    print("BENCHMARK 4 EXECUTION COMPLETE!")
    print("=" * 80)


if __name__ == "__main__":
    main()
