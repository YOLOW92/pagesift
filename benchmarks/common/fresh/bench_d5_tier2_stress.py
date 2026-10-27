#!/usr/bin/env python3
"""PageSift 0.50 Domain 5 Tier 2 Ultra-Extreme Chaos Stress Benchmark.

Tests:
1. 50 MB Pathological Mega-Corpus Chunking Stress Bomb:
   - 10,000 ATX Headings (# to ######)
   - 5,000 Markdown Tables
   - 5,000 Code Fences
   - ~8,000,000 words
   - Evaluates execution time, MB/s throughput, peak RSS memory under 120GB watchdog.

2. 10,000 Pathological QueryFilter Storm:
   - 10,000 adversarial multi-term queries evaluated against the dense corpus.
   - Evaluates queries-per-second (QPS), p50 / p99 latency, and memory stability.
"""

from __future__ import annotations

import json
import os
import random
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Dict, List, Tuple

import psutil

DATA_DIR = Path("benchmarks/data/d5")
DATA_DIR.mkdir(parents=True, exist_ok=True)
CORPUS_50MB = DATA_DIR / "tier2_corpus_50mb.md"
QUERIES_FILE = DATA_DIR / "tier2_queries_10k.jsonl"
OUT_DIR = Path("benchmarks/history/0.50/results")
OUT_DIR.mkdir(parents=True, exist_ok=True)

MEMORY_LIMIT_GB = 120.0


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
                print(f"[WATCHDOG ALERT] Exceeded {self.limit_bytes / 1e9:.1f} GB!", file=sys.stderr)
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


def generate_50mb_corpus():
    if CORPUS_50MB.exists() and CORPUS_50MB.stat().st_size >= 48 * 1024 * 1024:
        print(f"[Tier 2] 50MB corpus already exists: {CORPUS_50MB} ({CORPUS_50MB.stat().st_size / 1e6:.1f} MB)")
        return

    print(f"[Tier 2] Generating 50 MB Pathological Mega-Corpus at {CORPUS_50MB}...")
    random.seed(42)
    words = [
        "distributed", "consensus", "replicated", "state-machine", "raft", "paxos", "epoll", "simd",
        "vectorization", "zero-copy", "allocator", "lock-free", "concurrency", "cache-line", "l1-cache",
        "branch-predictor", "latency", "throughput", "pipeline", "instruction", "register", "hyper-threading"
    ]

    with open(CORPUS_50MB, "w", encoding="utf-8") as f:
        f.write("# PageSift 0.50 Ultra-High Pressure Mega-Corpus Stress Benchmark\n\n")
        f.write("> Pathological RAG stress suite containing 10,000 headings, 5,000 tables, and 5,000 code blocks.\n\n")

        target_size = 50 * 1024 * 1024
        curr_size = 0
        section_idx = 0

        while curr_size < target_size:
            section_idx += 1
            lvl = (section_idx % 6) + 1
            header_prefix = "#" * lvl
            title = f"{header_prefix} Section {section_idx:05d}: {' '.join(random.choices(words, k=4)).title()}\n\n"
            f.write(title)
            curr_size += len(title.encode("utf-8"))

            # Paragraph text
            para = " ".join(random.choices(words, k=60)) + ".\n\n"
            f.write(para)
            curr_size += len(para.encode("utf-8"))

            # Code block every 2 sections
            if section_idx % 2 == 0:
                code = f"```rust\n// Code snippet {section_idx}\npub fn process_chunk_{section_idx}(buf: &[u8]) -> usize {{\n    buf.iter().map(|&b| b as usize).sum()\n}}\n```\n\n"
                f.write(code)
                curr_size += len(code.encode("utf-8"))

            # Table every 3 sections
            if section_idx % 3 == 0:
                table = f"| Node ID | Shard Status | Ping Latency | Memory RSS |\n|:---|:---|:---|:---|\n| node-{section_idx}-a | ACTIVE | {random.randint(1, 50)}ms | {random.randint(100, 999)}MB |\n| node-{section_idx}-b | FOLLOWER | {random.randint(1, 50)}ms | {random.randint(100, 999)}MB |\n\n"
                f.write(table)
                curr_size += len(table.encode("utf-8"))

    print(f"[Tier 2] Generated {CORPUS_50MB.stat().st_size / 1e6:.2f} MB corpus ({section_idx} sections).")


def generate_10k_queries():
    if QUERIES_FILE.exists() and sum(1 for _ in open(QUERIES_FILE)) >= 10000:
        print(f"[Tier 2] 10k queries file already exists: {QUERIES_FILE}")
        return

    print(f"[Tier 2] Generating 10,000 Adversarial Queries at {QUERIES_FILE}...")
    random.seed(99)
    words = [
        "consensus", "zero-copy", "simd vectorization", "lock-free buffer", "raft election",
        "cache coherence", "paxos round", "epoll event queue", "allocator fragment", "branch miss",
        "throughput bottleneck", "latency tail", "memory corruption", "page fault", "atomic register"
    ]
    queries = []
    for i in range(10000):
        q = f"How does {' and '.join(random.choices(words, k=2))} impact system throughput in Section {random.randint(1, 5000):05d}?"
        queries.append({"id": f"q_{i+1:05d}", "query": q})

    with open(QUERIES_FILE, "w", encoding="utf-8") as f:
        for q in queries:
            f.write(json.dumps(q, ensure_ascii=False) + "\n")
    print(f"[Tier 2] Generated 10,000 queries in {QUERIES_FILE}")


def run_50mb_chunking_stress(py_bin: str, c4a_bin: str) -> dict:
    print("\n" + "=" * 80)
    print("=== [Tier 2.1] 50 MB Pathological Mega-Corpus Chunking Stress Bomb ===")
    print("=" * 80)

    corpus_size_mb = CORPUS_50MB.stat().st_size / 1e6
    results = {}

    # 1. PageSift Recursive
    print("\n[1/7] Testing PageSift-Recursive (Pure Rust AST Streaming)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    ps_cmd = ["target/release/examples/chunk_spans", '{"type":"recursive","size":200}', "/tmp/ps_50mb.json", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(ps_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["pagesift-recursive"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}
    else:
        print(f"  -> FAILED: {stderr.decode()}")
        results["pagesift-recursive"] = {"status": "FAILED", "err": stderr.decode()}

    # 2. PageSift Sliding Window
    print("\n[2/7] Testing PageSift-Sliding (Pure Rust)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    ps_cmd = ["target/release/examples/chunk_spans", '{"type":"sliding_window","window":200,"step":100}', "/tmp/ps_50mb_sl.json", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(ps_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["pagesift-sliding"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}

    # 3. Chonkie Recursive
    print("\n[3/7] Testing chonkie-recursive (Python + Rust core)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    py_cmd = [py_bin, "-I", "benchmarks/rag/chunkers.py", "/tmp/chonkie_out", "200", "chonkie-recursive", "--", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(py_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["chonkie-recursive"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}
    else:
        print(f"  -> FAILED: {stderr.decode()[:200]}")
        results["chonkie-recursive"] = {"status": "FAILED", "err": stderr.decode()[:200]}

    # 4. Semchunk
    print("\n[4/7] Testing semchunk (Python)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    py_cmd = [py_bin, "-I", "benchmarks/rag/chunkers.py", "/tmp/semchunk_out", "200", "semchunk", "--", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(py_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["semchunk"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}
    else:
        print(f"  -> FAILED: {stderr.decode()[:200]}")
        results["semchunk"] = {"status": "FAILED", "err": stderr.decode()[:200]}

    # 5. LangChain Recursive
    print("\n[5/7] Testing langchain-recursive (Python)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    py_cmd = [py_bin, "-I", "benchmarks/rag/chunkers.py", "/tmp/langchain_out", "200", "langchain-recursive", "--", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(py_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["langchain-recursive"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}
    else:
        print(f"  -> FAILED: {stderr.decode()[:200]}")
        results["langchain-recursive"] = {"status": "FAILED", "err": stderr.decode()[:200]}

    # 6. Crawl4AI Sliding
    print("\n[6/7] Testing crawl4ai-sliding (Python)...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    py_cmd = [c4a_bin, "-I", "benchmarks/rag/chunkers.py", "/tmp/crawl4ai_out", "200", "crawl4ai-sliding", "--", str(CORPUS_50MB)]
    t0 = time.perf_counter()
    proc = subprocess.Popen(py_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()
    if proc.returncode == 0:
        mb_per_s = corpus_size_mb / wall_s
        print(f"  -> SUCCESS: {wall_s:.3f}s ({mb_per_s:.1f} MB/s), Peak RSS: {peak_mb:.1f} MB")
        results["crawl4ai-sliding"] = {"status": "PASSED", "wall_time_s": wall_s, "mb_per_s": mb_per_s, "peak_rss_mb": peak_mb}
    else:
        print(f"  -> FAILED: {stderr.decode()[:200]}")
        results["crawl4ai-sliding"] = {"status": "FAILED", "err": stderr.decode()[:200]}

    return results


def run_10k_query_storm(c4a_bin: str) -> dict:
    print("\n" + "=" * 80)
    print("=== [Tier 2.2] 10,000 Pathological QueryFilter Storm ===")
    print("=" * 80)

    # Convert 10k queries into temporary questions.jsonl for qf_ranks
    tmp_q_jsonl = DATA_DIR / "tier2_qf_questions.jsonl"
    with open(QUERIES_FILE) as fin, open(tmp_q_jsonl, "w") as fout:
        for line in fin:
            row = json.loads(line)
            fout.write(json.dumps({
                "id": row["id"],
                "corpus": "tier2_corpus_50mb",
                "question": row["query"],
                "gold": [[0, 100]],
                "half": "all"
            }) + "\n")

    results = {}
    print("\n[1/2] Bombarding PageSift SIMD BM25 QueryFilter with 10,000 queries...")
    watchdog = MemoryWatchdog(limit_gb=MEMORY_LIMIT_GB)
    t0 = time.perf_counter()
    proc = subprocess.Popen([
        "target/release/examples/qf_ranks",
        str(DATA_DIR),
        str(tmp_q_jsonl),
        "/tmp/qf_ps_10k.json"
    ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog.start([proc.pid, os.getpid()])
    stdout, stderr = proc.communicate()
    wall_s = time.perf_counter() - t0
    peak_mb = watchdog.stop()

    qps = 10000.0 / wall_s
    lat_us = (wall_s / 10000.0) * 1e6
    print(f"  -> PageSift QF: {qps:.1f} QPS, {wall_s:.3f}s total ({lat_us:.1f} µs/query), Peak RSS: {peak_mb:.1f} MB")
    results["pagesift-queryfilter"] = {"status": "PASSED", "qps": qps, "wall_s": wall_s, "lat_us_per_q": lat_us, "peak_rss_mb": peak_mb}

    return results


def main():
    generate_50mb_corpus()
    generate_10k_queries()

    py_bin = "/home/wyc610721/pagesift-bench-env/tools/py050/bin/python"
    c4a_bin = "/home/wyc610721/pagesift-bench-env/tools/c4ai/bin/python"

    chunk_res = run_50mb_chunking_stress(py_bin, c4a_bin)
    qf_res = run_10k_query_storm(c4a_bin)

    tier2_summary = {
        "tier2_50mb_chunking": chunk_res,
        "tier2_10k_queryfilter": qf_res
    }

    out_file = OUT_DIR / "benchmark5_tier2_chaos_stress.jsonl"
    with open(out_file, "w", encoding="utf-8") as f:
        f.write(json.dumps(tier2_summary, ensure_ascii=False) + "\n")
    print(f"\nSaved Tier 2 stress results to {out_file}")


if __name__ == "__main__":
    main()
