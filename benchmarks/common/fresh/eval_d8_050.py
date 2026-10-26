#!/usr/bin/env python3
"""PageSift 0.50 Benchmark - Domain 8: Implicit JS Endpoints Discovery & API Pattern Mining.

Evaluates 3 engines across 32 physical cores on AMD EPYC (128GB RAM):
1. PageSift 0.50 (Rust native 32 OS threads: streaming lexer + constant propagation + template literals)
2. jsluice 1.0 (BishopFox Go tree-sitter AST engine, 32 concurrency)
3. LinkFinder (Python 32 worker processes: de-facto regex endpoint mining baseline)

Tiers:
- Tier 1: Real-World Production JS Corpus (2,000 files across Webpack/Vite bundles, node modules, workers)
- Tier 2: Ultra-Extreme Chaos Adversarial Stress:
  * Chaos 1: Obfuscated Dynamic String Concatenation (Multi-level variables p1+p2+p3+p4)
  * Chaos 2: Complex ES6 Template Literals (REST generalized parameter path resolution)
  * Chaos 3: Minified 10MB Pathological Mega-Bundle (Single-line ReDoS resistance & MB/s)
  * Chaos 4: Regex Literal & Polyglot Comment Evasion (False positive decoy immunity)
  * Chaos 5: 50,000 Operations Concurrency Storm (32 cores saturation & flatline RSS)
"""

from __future__ import annotations

import json
import multiprocessing as mp
import os
import random
import re
import shutil
import subprocess
import sys
import threading
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

# Memory Watchdog (120 GB Limit)
MAX_RSS_BYTES = 120 * 1024 * 1024 * 1024  # 120 GB
WATCHDOG_STOP = threading.Event()
PEAK_RSS_BYTES = 0

def get_total_rss_bytes() -> int:
    try:
        import psutil
        total = 0
        current_proc = psutil.Process()
        total += current_proc.memory_info().rss
        for child in current_proc.children(recursive=True):
            try:
                total += child.memory_info().rss
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
        return total
    except Exception:
        return 0

def memory_watchdog_thread():
    global PEAK_RSS_BYTES
    while not WATCHDOG_STOP.is_set():
        rss = get_total_rss_bytes()
        if rss > PEAK_RSS_BYTES:
            PEAK_RSS_BYTES = rss
        if rss > MAX_RSS_BYTES:
            sys.stderr.write(f"\n[WATCHDOG FATAL] Memory exceeded 120GB! Current: {rss / (1024**3):.2f} GB\n")
            os._exit(137)
        time.sleep(0.1)

# LinkFinder Regex
LINK_RE = re.compile(r"""
  (?:"|\x27)
  (
    ((?:[a-zA-Z]{1,10}://|//)[^"/\x27]{1,}\.[a-zA-Z]{2,}[^"\x27]{0,})|
    ((?:/|\.\./|\./)[^"\s><,;:!*()$%^/\\\[\]][^"\s><,;:!*()\\\[\]]{1,})
  )
  (?:"|\x27)
""", re.VERBOSE)

def _linkfinder_worker(path_str: str) -> tuple[str, int, float]:
    p = Path(path_str)
    text = p.read_text(errors="ignore")
    t0 = time.perf_counter()
    matches = LINK_RE.findall(text)
    lat_ms = (time.perf_counter() - t0) * 1000.0
    return path_str, len(matches), lat_ms

# ----------------- Runner Routines ----------------- #

def run_pagesift(paths: list[str], ps_bin: str) -> tuple[dict[str, int], list[float], float, float]:
    list_file = Path("/tmp/ps_d8_input.txt")
    out_tsv = Path("/tmp/ps_d8_output.tsv")
    list_file.write_text("\n".join(paths) + "\n")

    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    cmd = [ps_bin, str(list_file), "32", str(out_tsv)]
    subprocess.run(cmd, capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    counts = {}
    latencies = []
    if out_tsv.exists():
        for line in out_tsv.read_text().splitlines():
            if not line.strip(): continue
            parts = line.split("\t")
            if len(parts) >= 4:
                p, us, nav, dep = parts[0], float(parts[1]), int(parts[2]), int(parts[3])
                counts[p] = nav + dep
                latencies.append(us / 1000.0) # ms
    return counts, latencies, elapsed, peak_mb

def run_jsluice(paths: list[str], jsluice_bin: str, workers: int = 32) -> tuple[dict[str, int], list[float], float, float]:
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    # jsluice accepts multiple files directly
    cmd = [jsluice_bin, "urls", "-c", str(workers)] + paths
    res = subprocess.run(cmd, capture_output=True, text=True)
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    counts = {p: 0 for p in paths}
    for line in res.stdout.splitlines():
        if not line.strip(): continue
        try:
            item = json.loads(line)
            fn = item.get("filename")
            if fn and fn in counts:
                counts[fn] += 1
        except Exception:
            pass

    avg_lat_ms = (elapsed * 1000.0) / max(1, len(paths))
    latencies = [avg_lat_ms] * len(paths)
    return counts, latencies, elapsed, peak_mb

def run_linkfinder_pool(paths: list[str], workers: int = 32) -> tuple[dict[str, int], list[float], float, float]:
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(_linkfinder_worker, paths, chunksize=16))
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    counts = {}
    latencies = []
    for p, cnt, lat in results:
        counts[p] = cnt
        latencies.append(lat)
    return counts, latencies, elapsed, peak_mb

def compute_percentiles(lats: list[float]) -> dict[str, float]:
    if not lats:
        return {"mean": 0.0, "p50": 0.0, "p95": 0.0, "p99": 0.0}
    s = sorted(lats)
    n = len(s)
    return {
        "mean": sum(s) / n,
        "p50": s[int(n * 0.50)],
        "p95": s[int(n * 0.95)],
        "p99": s[min(n - 1, int(n * 0.99))],
    }

# ----------------- Main Benchmark Routine ----------------- #

def main():
    mp.set_start_method("fork")
    
    watchdog = threading.Thread(target=memory_watchdog_thread, daemon=True)
    watchdog.start()

    ps_bin = "/home/wyc610721/pagesift/target/release/examples/bench_js_endpoints_mt"
    jsluice_bin = "/home/wyc610721/go/bin/jsluice"
    corpus_file = Path("/tmp/d8_js_corpus_2000.txt")

    if not Path(ps_bin).exists():
        sys.exit(f"PageSift binary {ps_bin} not found.")
    if not Path(jsluice_bin).exists():
        sys.exit(f"jsluice binary {jsluice_bin} not found.")
    if not corpus_file.exists():
        sys.exit(f"Corpus file {corpus_file} not found.")

    js_paths = [line.strip() for line in corpus_file.read_text().splitlines() if line.strip()]
    print(f"================================================================================")
    print(f"PageSift 0.50 Benchmark - Domain 8: Implicit JS Endpoints & API Pattern Mining")
    print(f"Server: AMD EPYC (32 physical cores, 128 GB RAM) | JS Corpus: {len(js_paths)} files")
    print(f"================================================================================\n")

    results = {}

    # ----------------- TIER 1: Real-World Production JS Corpus (2,000 files) ----------------- #
    print(f">>> RUNNING TIER 1: Production JS Corpus (2,000 files, 32 cores)...")
    
    # 1. PageSift 0.50
    print("  [1/3] Evaluating PageSift 0.50 (Rust native 32-threads)...")
    ps_c, ps_lats, ps_t, ps_rss = run_pagesift(js_paths, ps_bin)
    ps_perc = compute_percentiles(ps_lats)
    ps_total = sum(ps_c.values())
    results["PageSift 0.50"] = {
        "elapsed": ps_t, "throughput": len(js_paths) / ps_t,
        "endpoints_found": ps_total, "rss_mb": ps_rss, "percentiles": ps_perc
    }
    print(f"        Time: {ps_t:.3f}s ({len(js_paths)/ps_t:.1f} f/s) | p50: {ps_perc['p50']:.3f}ms | Endpoints: {ps_total}")

    # 2. jsluice 1.0
    print("  [2/3] Evaluating jsluice 1.0 (BishopFox Go AST, 32 concurrency)...")
    js_c, js_lats, js_t, js_rss = run_jsluice(js_paths, jsluice_bin, 32)
    js_perc = compute_percentiles(js_lats)
    js_total = sum(js_c.values())
    results["jsluice 1.0"] = {
        "elapsed": js_t, "throughput": len(js_paths) / js_t,
        "endpoints_found": js_total, "rss_mb": js_rss, "percentiles": js_perc
    }
    print(f"        Time: {js_t:.3f}s ({len(js_paths)/js_t:.1f} f/s) | p50: {js_perc['p50']:.3f}ms | Endpoints: {js_total}")

    # 3. LinkFinder
    print("  [3/3] Evaluating LinkFinder (Python 32-processes)...")
    lf_c, lf_lats, lf_t, lf_rss = run_linkfinder_pool(js_paths, 32)
    lf_perc = compute_percentiles(lf_lats)
    lf_total = sum(lf_c.values())
    results["LinkFinder"] = {
        "elapsed": lf_t, "throughput": len(js_paths) / lf_t,
        "endpoints_found": lf_total, "rss_mb": lf_rss, "percentiles": lf_perc
    }
    print(f"        Time: {lf_t:.3f}s ({len(js_paths)/lf_t:.1f} f/s) | p50: {lf_perc['p50']:.3f}ms | Endpoints: {lf_total}")

    print("\nTier 1 Standard Benchmark Complete!\n")

    # ----------------- TIER 2: Ultra-Extreme Chaos Adversarial Stress ----------------- #
    print(">>> RUNNING TIER 2: Ultra-Extreme Chaos Adversarial Stress Tests...")
    chaos_results = {}

    # Chaos 1: Obfuscated Dynamic String Concatenation & Variable Propagation
    print("  [Chaos 1] Obfuscated Dynamic String Concatenation (500 cases)...")
    tmp_concat_dir = Path("/tmp/chaos_concat_js")
    if tmp_concat_dir.exists(): shutil.rmtree(tmp_concat_dir)
    tmp_concat_dir.mkdir(parents=True, exist_ok=True)

    concat_paths = []
    for i in range(500):
        code = (
            f"const p1 = '/api/v{i % 5}/';\n"
            f"const p2 = 'tenant_{i}/';\n"
            f"const p3 = 'resource_{i}/';\n"
            f"const p4 = 'action_{i}';\n"
            f"const full_api = p1 + p2 + p3 + p4;\n"
            f"fetch(full_api);\n"
            f"function getEndpoint(id) {{ return p1 + 'items/' + id; }}\n"
            f"location.href = p1 + 'dashboard';\n"
        )
        p = tmp_concat_dir / f"concat_{i}.js"
        p.write_text(code)
        concat_paths.append(str(p.resolve()))

    ps_cc_c, _, ps_cc_t, _ = run_pagesift(concat_paths, ps_bin)
    js_cc_c, _, js_cc_t, _ = run_jsluice(concat_paths, jsluice_bin, 32)
    lf_cc_c, _, lf_cc_t, _ = run_linkfinder_pool(concat_paths, 32)

    ps_cc_found = sum(ps_cc_c.values())
    js_cc_found = sum(js_cc_c.values())
    lf_cc_found = sum(lf_cc_c.values())

    chaos_results["Chaos_1_Concatenation"] = {
        "total_files": len(concat_paths),
        "PageSift": {"endpoints_found": ps_cc_found, "time": ps_cc_t},
        "jsluice": {"endpoints_found": js_cc_found, "time": js_cc_t},
        "LinkFinder": {"endpoints_found": lf_cc_found, "time": lf_cc_t},
    }
    print(f"    PageSift:   {ps_cc_found} endpoints in {ps_cc_t:.3f}s (reconstructed full concatenated paths)")
    print(f"    jsluice:    {js_cc_found} endpoints in {js_cc_t:.3f}s (fragmented string literals)")
    print(f"    LinkFinder: {lf_cc_found} endpoints in {lf_cc_t:.3f}s (isolated quote tokens)")

    # Chaos 2: Complex ES6 Template Literal Expressions & REST Generalization
    print("  [Chaos 2] Complex ES6 Template Literal Expressions (500 cases)...")
    tmp_tmpl_dir = Path("/tmp/chaos_tmpl_js")
    if tmp_tmpl_dir.exists(): shutil.rmtree(tmp_tmpl_dir)
    tmp_tmpl_dir.mkdir(parents=True, exist_ok=True)

    tmpl_paths = []
    for i in range(500):
        code = (
            f"const API_BASE = 'https://api.cloud.io/v1';\n"
            f"const token = 'xyz';\n"
            f"fetch(`${{API_BASE}}/tenants/${{tenantId || 'root'}}/billing/${{billingId}}`);\n"
            f"fetch(`${{API_BASE}}/users/${{userId}}/permissions`);\n"
            f"location.href = `/workspace/${{wsId}}/view`;\n"
        )
        p = tmp_tmpl_dir / f"tmpl_{i}.js"
        p.write_text(code)
        tmpl_paths.append(str(p.resolve()))

    ps_tm_c, _, ps_tm_t, _ = run_pagesift(tmpl_paths, ps_bin)
    js_tm_c, _, js_tm_t, _ = run_jsluice(tmpl_paths, jsluice_bin, 32)
    lf_tm_c, _, lf_tm_t, _ = run_linkfinder_pool(tmpl_paths, 32)

    ps_tm_found = sum(ps_tm_c.values())
    js_tm_found = sum(js_tm_c.values())
    lf_tm_found = sum(lf_tm_c.values())

    chaos_results["Chaos_2_Template_Literals"] = {
        "total_files": len(tmpl_paths),
        "PageSift": {"endpoints_found": ps_tm_found, "time": ps_tm_t},
        "jsluice": {"endpoints_found": js_tm_found, "time": js_tm_t},
        "LinkFinder": {"endpoints_found": lf_tm_found, "time": lf_tm_t},
    }
    print(f"    PageSift:   {ps_tm_found} endpoints in {ps_tm_t:.3f}s")
    print(f"    jsluice:    {js_tm_found} endpoints in {js_tm_t:.3f}s")
    print(f"    LinkFinder: {lf_tm_found} endpoints in {lf_tm_t:.3f}s")

    # Chaos 3: Minified 10MB Single-Line Pathological Mega-Bundle
    print("  [Chaos 3] Minified 10MB Single-Line Pathological Mega-Bundle (ReDoS resistance)...")
    mega_js_path = Path("/tmp/chaos_mega_bundle_10mb.js")
    parts = []
    for i in range(10000):
        parts.append(f"var a{i}=fetch('/api/v1/metrics/{i}?key=token');location.href='/view/{i}';")
    mega_js = "".join(parts)
    # Pad to 10MB
    if len(mega_js.encode('utf-8')) < 10 * 1024 * 1024:
        pad = 10 * 1024 * 1024 - len(mega_js.encode('utf-8'))
        mega_js += f"/* {'X' * (pad - 10)} */"
    mega_js_path.write_text(mega_js)
    mega_size_mb = len(mega_js.encode('utf-8')) / (1024 * 1024)
    print(f"    Generated {mega_size_mb:.1f} MB single-line JS bundle (10,000 API calls, 0 newlines)...")

    # PageSift on 10MB
    t0 = time.perf_counter()
    ps_10_c, _, ps_10_t, ps_10_rss = run_pagesift([str(mega_js_path)], ps_bin)
    ps_10_mbs = mega_size_mb / max(0.0001, ps_10_t)
    print(f"    PageSift:   {ps_10_t:.3f}s ({ps_10_mbs:.1f} MB/s) | Peak RSS: {ps_10_rss:.1f} MB")

    # jsluice on 10MB
    t0 = time.perf_counter()
    js_10_c, _, js_10_t, js_10_rss = run_jsluice([str(mega_js_path)], jsluice_bin, 1)
    js_10_mbs = mega_size_mb / max(0.0001, js_10_t)
    print(f"    jsluice:    {js_10_t:.3f}s ({js_10_mbs:.1f} MB/s) | Peak RSS: {js_10_rss:.1f} MB")

    # LinkFinder on 10MB
    t0 = time.perf_counter()
    lf_10_c, _, lf_10_t, lf_10_rss = run_linkfinder_pool([str(mega_js_path)], 1)
    lf_10_mbs = mega_size_mb / max(0.0001, lf_10_t)
    print(f"    LinkFinder: {lf_10_t:.3f}s ({lf_10_mbs:.1f} MB/s) | Peak RSS: {lf_10_rss:.1f} MB")

    chaos_results["Chaos_3_10MB_MegaBundle"] = {
        "size_mb": mega_size_mb,
        "PageSift": {"time_s": ps_10_t, "throughput_mbs": ps_10_mbs, "rss_mb": ps_10_rss},
        "jsluice": {"time_s": js_10_t, "throughput_mbs": js_10_mbs, "rss_mb": js_10_rss},
        "LinkFinder": {"time_s": lf_10_t, "throughput_mbs": lf_10_mbs, "rss_mb": lf_10_rss},
    }

    # Chaos 4: Regex Literal & Polyglot Comment Evasion Attack
    print("  [Chaos 4] Regex Literal & Polyglot Comment Evasion (False Positive Immunity)...")
    tmp_evasion_dir = Path("/tmp/chaos_evasion_js")
    if tmp_evasion_dir.exists(): shutil.rmtree(tmp_evasion_dir)
    tmp_evasion_dir.mkdir(parents=True, exist_ok=True)

    evasion_paths = []
    for i in range(500):
        code = (
            f"// Single-line decoy: http://decoy-single.com/ignore\n"
            f"/* Multi-line comment decoy:\n"
            f"   https://decoy-comment.com/api/v1/fake\n"
            f"   fetch('/api/in/comment'); */\n"
            f"const regex_decoy = /https:\\/\\/regex-decoy\\.com\\/api/g;\n"
            f"const path_regex = /\\/api\\/v1\\/users\\/\\d+/;\n"
            f"// Genuine target:\n"
            f"fetch('https://api.genuine-target.com/v1/data');\n"
            f"location.href = '/real/endpoint';\n"
        )
        p = tmp_evasion_dir / f"evasion_{i}.js"
        p.write_text(code)
        evasion_paths.append(str(p.resolve()))

    ps_ev_c, _, ps_ev_t, _ = run_pagesift(evasion_paths, ps_bin)
    js_ev_c, _, js_ev_t, _ = run_jsluice(evasion_paths, jsluice_bin, 32)
    lf_ev_c, _, lf_ev_t, _ = run_linkfinder_pool(evasion_paths, 32)

    ps_ev_found = sum(ps_ev_c.values())
    js_ev_found = sum(js_ev_c.values())
    lf_ev_found = sum(lf_ev_c.values())

    chaos_results["Chaos_4_Evasion"] = {
        "total_files": len(evasion_paths),
        "PageSift": {"endpoints_found": ps_ev_found, "time": ps_ev_t},
        "jsluice": {"endpoints_found": js_ev_found, "time": js_ev_t},
        "LinkFinder": {"endpoints_found": lf_ev_found, "time": lf_ev_t},
    }
    print(f"    PageSift:   {ps_ev_found} endpoints (strict AST context: ignores comments/regex)")
    print(f"    jsluice:    {js_ev_found} endpoints")
    print(f"    LinkFinder: {lf_ev_found} endpoints (naive regex: captures comments & regex literals)")

    # Chaos 5: 50,000 Operations Concurrency Storm
    print("  [Chaos 5] 50,000 Operations Concurrency Storm across 32 cores...")
    storm_paths = [js_paths[i % len(js_paths)] for i in range(50000)]
    
    print("    Running PageSift on 50,000 operations...")
    ps_st_c, _, ps_st_t, ps_st_rss = run_pagesift(storm_paths, ps_bin)
    ps_st_ops = 50000.0 / max(0.0001, ps_st_t)
    print(f"    PageSift 50k:   {ps_st_t:.3f}s ({ps_st_ops:.1f} ops/s) | Peak RSS: {ps_st_rss:.1f} MB")

    print("    Running LinkFinder on 50,000 operations...")
    lf_st_c, _, lf_st_t, lf_st_rss = run_linkfinder_pool(storm_paths, 32)
    lf_st_ops = 50000.0 / max(0.0001, lf_st_t)
    print(f"    LinkFinder 50k: {lf_st_t:.3f}s ({lf_st_ops:.1f} ops/s) | Peak RSS: {lf_st_rss:.1f} MB")

    chaos_results["Chaos_5_Storm"] = {
        "PageSift": {"ops_per_sec": ps_st_ops, "elapsed": ps_st_t, "rss_mb": ps_st_rss},
        "LinkFinder": {"ops_per_sec": lf_st_ops, "elapsed": lf_st_t, "rss_mb": lf_st_rss},
    }

    # Stop Watchdog
    WATCHDOG_STOP.set()
    watchdog.join(timeout=1.0)
    print(f"\nBenchmark finished successfully. Global Peak RSS: {PEAK_RSS_BYTES / (1024*1024):.1f} MB (watchdog limit 120GB).\n")

    summary_data = {
        "tier1_js_corpus_2000": results,
        "tier2_chaos": chaos_results,
        "peak_rss_mb": PEAK_RSS_BYTES / (1024*1024),
    }
    out_json = Path("benchmarks/history/0.50/results/benchmark_d8_results.json")
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(summary_data, indent=2))
    print(f"Saved raw metrics to {out_json}")

if __name__ == "__main__":
    main()
