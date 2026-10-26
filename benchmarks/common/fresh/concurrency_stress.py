#!/usr/bin/env python3
"""Concurrency & Thread Scalability Extreme Stress Test.

Measures PageSift vs rs-trafilatura across 1, 2, 4, 8, 16 threads on the full 2,008-page WCXB dataset.
Tracks:
- Wall clock time (s)
- Throughput (pages/sec)
- Peak Memory (RSS in MB)
- CPU Efficiency (User CPU time)
- Latency Percentiles (p50, p95, p99)
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from pathlib import Path

MANIFEST = "bench/data/fresh/results/wcxb_all/manifest.jsonl"
BIN = "bench/compare/rust-tools/target/release/pagesift-compare-rust"


def run_with_threads(tool: str, threads: int, out_file: str) -> dict:
    env = os.environ.copy()
    if threads == 1:
        env["PAGESIFT_ONE_CORE"] = "1"
        env["RAYON_NUM_THREADS"] = "1"
    else:
        env.pop("PAGESIFT_ONE_CORE", None)
        env["RAYON_NUM_THREADS"] = str(threads)

    cmd = ["/usr/bin/time", "-v", BIN, tool, MANIFEST, out_file]
    t0 = time.perf_counter()
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True)
    elapsed = time.perf_counter() - t0

    # Parse /usr/bin/time output from stderr
    stderr = proc.stderr
    user_time = 0.0
    rss_kb = 0
    m_user = re.search(r"User time \(seconds\):\s*([\d\.]+)", stderr)
    if m_user:
        user_time = float(m_user.group(1))
    m_rss = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
    if m_rss:
        rss_kb = int(m_rss.group(1))

    # Parse per-page latencies from out_file
    latencies = []
    if Path(out_file).exists():
        with open(out_file) as f:
            for line in f:
                try:
                    import json
                    row = json.loads(line)
                    if "ms" in row:
                        latencies.append(float(row["ms"]))
                except Exception:
                    continue

    latencies.sort()
    n = len(latencies)
    p50 = latencies[n // 2] if n else 0.0
    p95 = latencies[int(n * 0.95)] if n else 0.0
    p99 = latencies[int(n * 0.99)] if n else 0.0

    return {
        "tool": tool,
        "threads": threads,
        "elapsed": elapsed,
        "pages_sec": n / elapsed if elapsed > 0 else 0,
        "user_time": user_time,
        "rss_mb": rss_kb / 1024.0,
        "p50": p50,
        "p95": p95,
        "p99": p99,
        "count": n,
    }


def main():
    print("================================================================================")
    print("=== MULTI-THREAD CONCURRENCY & EXTREME THROUGHPUT STRESS TEST (2,008 PAGES) ===")
    print("================================================================================\n")

    thread_counts = [1, 2, 4, 8, 16]

    print("Running PageSift concurrency scaling...")
    ps_results = []
    for tc in thread_counts:
        out_f = f"/tmp/ps_t{tc}.jsonl"
        r = run_with_threads("pagesift", tc, out_f)
        ps_results.append(r)
        print(f"  PageSift @ {tc:2d} threads: {r['elapsed']:.2f}s ({r['pages_sec']:6.0f} pages/s) | p50: {r['p50']:5.2f}ms | p99: {r['p99']:5.2f}ms | RSS: {r['rss_mb']:5.1f}MB")

    print("\nRunning rs-trafilatura concurrency scaling...")
    rs_results = []
    for tc in [1, 4, 8]:
        out_f = f"/tmp/rs_t{tc}.jsonl"
        r = run_with_threads("rs-trafilatura", tc, out_f)
        rs_results.append(r)
        print(f"  rs-trafilatura @ {tc:2d} threads: {r['elapsed']:.2f}s ({r['pages_sec']:6.0f} pages/s) | p50: {r['p50']:5.2f}ms | p99: {r['p99']:5.2f}ms | RSS: {r['rss_mb']:5.1f}MB")

    print("\n" + "=" * 80)
    print(f"{'Threads':<9} {'Engine':<16} {'Wall Time':<12} {'Throughput':<16} {'p50 Latency':<14} {'Peak RSS':<10}")
    print("=" * 80)
    for ps in ps_results:
        print(f"{ps['threads']:<9} {'PageSift':<16} {ps['elapsed']:<12.2f}s {ps['pages_sec']:<14.0f} p/s {ps['p50']:<12.2f}ms {ps['rss_mb']:<8.1f}MB")
    print("-" * 80)
    for rs in rs_results:
        print(f"{rs['threads']:<9} {'rs-trafilatura':<16} {rs['elapsed']:<12.2f}s {rs['pages_sec']:<14.0f} p/s {rs['p50']:<12.2f}ms {rs['rss_mb']:<8.1f}MB")
    print("================================================================================\n")


if __name__ == "__main__":
    main()
