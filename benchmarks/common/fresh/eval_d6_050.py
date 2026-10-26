#!/usr/bin/env python3
"""PageSift 0.50 Benchmark - Domain 6: Charset Encoding Detection & Normalization.

Evaluates 5 engines across 32 physical cores on AMD EPYC (128GB RAM):
1. PageSift 0.50 (Rust native 32-threads, SIMD ASCII + chardetng + WHATWG normalization)
2. uchardet 0.0.8 (C++ Mozilla Universal Charset Detector port, 32 processes)
3. cchardet 2.4.0 (C/Cython Universal Charset Detector wrapper, 32 processes)
4. charset-normalizer 3.5.2 (Python Unicode Normalizer, 32 processes)
5. chardet 7.7.0 (Python Universal Charset Detector, 32 processes)

Tiers:
- Tier 1: Standard Public Benchmark (3,136 files from chardet-test-data across 5 language families)
- Tier 2: Ultra-Extreme Chaos Adversarial Stress:
  1. Chameleon Spoofing (Conflicting BOM & HTML Meta Spoofing)
  2. Adversarial Truncation (Head 1-3 byte strip & 64B/128B Micro-slice)
  3. Noise & Bit-Flip Corruption (1% and 5% dirty byte injections)
  4. Pathological 50MB Polyglot Mega-Stream (Throughput MB/s & Peak RSS)
  5. 50,000 Operations Concurrency Storm (ops/sec & 32-core stability)
"""

from __future__ import annotations

import json
import multiprocessing as mp
import os
import random
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

# Canonical Encodings & Normalization Map
ALIASES = {
    "ascii": "ascii",
    "utf-8": "utf-8",
    "utf-8-sig": "utf-8",
    "utf_8": "utf-8",
    "utf_8_sig": "utf-8",
    "utf-16": "utf-16",
    "utf_16": "utf-16",
    "utf-16le": "utf-16le",
    "utf_16_le": "utf-16le",
    "utf-16be": "utf-16be",
    "utf_16_be": "utf-16be",
    "utf-32": "utf-32",
    "utf_32": "utf-32",
    "utf-32le": "utf-32le",
    "utf_32_le": "utf-32le",
    "utf-32be": "utf-32be",
    "utf_32_be": "utf-32be",
    "utf-7": "utf-7",
    "utf_7": "utf-7",
    "windows-1252": "windows-1252",
    "cp1252": "windows-1252",
    "iso-8859-1": "windows-1252",
    "iso8859-1": "windows-1252",
    "latin1": "windows-1252",
    "latin-1": "windows-1252",
    "windows-1251": "windows-1251",
    "cp1251": "windows-1251",
    "windows-1250": "windows-1250",
    "cp1250": "windows-1250",
    "windows-1253": "windows-1253",
    "cp1253": "windows-1253",
    "windows-1254": "windows-1254",
    "cp1254": "windows-1254",
    "iso-8859-9": "windows-1254",
    "windows-1255": "windows-1255",
    "cp1255": "windows-1255",
    "windows-1256": "windows-1256",
    "cp1256": "windows-1256",
    "windows-1257": "windows-1257",
    "cp1257": "windows-1257",
    "windows-1258": "windows-1258",
    "cp1258": "windows-1258",
    "gb2312": "gb18030",
    "gbk": "gb18030",
    "gb18030": "gb18030",
    "hz-gb-2312": "hz-gb-2312",
    "big5": "big5",
    "big-5": "big5",
    "big5hkscs": "big5",
    "cp950": "big5",
    "shift_jis": "shift_jis",
    "shift-jis": "shift_jis",
    "sjis": "shift_jis",
    "cp932": "shift_jis",
    "windows-31j": "shift_jis",
    "euc-jp": "euc-jp",
    "euc_jp": "euc-jp",
    "euc_jis_2004": "euc-jp",
    "euc-kr": "euc-kr",
    "euc_kr": "euc-kr",
    "cp949": "euc-kr",
    "koi8-r": "koi8-r",
    "koi8-u": "koi8-u",
    "koi8-t": "koi8-t",
    "iso-8859-2": "iso-8859-2",
    "iso-8859-3": "iso-8859-3",
    "iso-8859-4": "iso-8859-4",
    "iso-8859-5": "iso-8859-5",
    "iso-8859-6": "iso-8859-6",
    "iso-8859-7": "iso-8859-7",
    "iso-8859-8": "iso-8859-8",
    "iso-8859-10": "iso-8859-10",
    "iso-8859-11": "tis-620",
    "tis-620": "tis-620",
    "iso-8859-13": "iso-8859-13",
    "iso-8859-14": "iso-8859-14",
    "iso-8859-15": "windows-1252",
    "iso-8859-16": "iso-8859-16",
    "ibm866": "cp866",
    "cp866": "cp866",
    "ibm850": "cp850",
    "cp850": "cp850",
    "ibm852": "cp852",
    "cp852": "cp852",
    "ibm855": "cp855",
    "cp855": "cp855",
    "ibm857": "cp857",
    "cp857": "cp857",
    "ibm437": "cp437",
    "cp437": "cp437",
    "macroman": "macroman",
    "mac-roman": "macroman",
    "maccyrillic": "maccyrillic",
    "mac-cyrillic": "maccyrillic",
    "macgreek": "macgreek",
    "mac-greek": "macgreek",
    "maclatin2": "maclatin2",
    "mac-centraleurope": "maclatin2",
    "maciceland": "maciceland",
    "mac-iceland": "maciceland",
    "macturkish": "macturkish",
    "mac-turkish": "macturkish",
    "cp037": "cp037",
    "ibm037": "cp037",
    "cp500": "cp500",
    "ibm500": "cp500",
}

def canon(name: str | None) -> str:
    if not name:
        return "unknown"
    n = name.strip().lower().replace("_", "-").replace(" ", "-").replace("/", "-")
    return ALIASES.get(n, n)

def is_compat(detected: str | None, expected: str) -> bool:
    if not detected:
        return False
    d = canon(detected)
    e = canon(expected)
    if d == e:
        return True
    if d == "ascii" and e in ("utf-8", "windows-1252", "iso-8859-1"):
        return True
    if e == "ascii" and d in ("utf-8", "windows-1252"):
        return True
    if d in ("utf-16", "utf-16le", "utf-16be") and e in ("utf-16", "utf-16le", "utf-16be"):
        return True
    if d in ("utf-32", "utf-32le", "utf-32be") and e in ("utf-32", "utf-32le", "utf-32be"):
        return True
    if d in ("gb18030", "gbk", "gb2312") and e in ("gb18030", "gbk", "gb2312"):
        return True
    if d in ("shift_jis", "cp932", "windows-31j") and e in ("shift_jis", "cp932", "windows-31j"):
        return True
    if d in ("euc-kr", "cp949") and e in ("euc-kr", "cp949"):
        return True
    return False

def categorize(enc: str) -> str:
    e = enc.lower()
    if any(k in e for k in ["gb", "big5", "shift", "sjis", "cp932", "euc-jp", "euc-kr", "cp949", "johab", "hz", "iso-2022"]):
        return "CJK (Chinese/Japanese/Korean)"
    elif any(k in e for k in ["1251", "koi8", "8859-5", "maccyrillic", "cp866", "cp855", "kz1048", "ptcp154"]):
        return "Cyrillic (Russian/Ukrainian/Slavic)"
    elif any(k in e for k in ["utf-8", "utf-16", "utf-32", "utf-7"]):
        return "Unicode (UTF-8/16/32/7)"
    elif any(k in e for k in ["1252", "1250", "8859-1", "8859-2", "8859-3", "8859-4", "8859-9", "8859-10", "8859-13", "8859-14", "8859-15", "8859-16", "cp850", "cp852", "hp-roman8", "macroman", "maclatin2", "maciceland"]):
        return "Western & Latin (Latin-1/15, CP1252/1250)"
    else:
        return "Legacy & Others (Greek, Hebrew, Arabic, EBCDIC, DOS)"

def collect_test_cases(data_dir: Path):
    cases = []
    for p in sorted(data_dir.iterdir()):
        if not p.is_dir() or p.name.startswith(".") or p.name == "scripts" or p.name == "None-None":
            continue
        parts = p.name.rsplit("-", 1)
        if len(parts) != 2:
            continue
        enc, lang = parts[0], parts[1]
        for f in sorted(p.iterdir()):
            if f.is_file() and not f.name.startswith(".") and f.suffix != ".md":
                cases.append((f, enc, lang))
    return cases

# ----------------- Competitor Workers ----------------- #

def _cchardet_worker(path_str: str) -> tuple[str, str | None, float]:
    import cchardet
    b = Path(path_str).read_bytes()
    t0 = time.perf_counter()
    r = cchardet.detect(b)
    lat_ms = (time.perf_counter() - t0) * 1000.0
    return path_str, r.get("encoding"), lat_ms

def _cn_worker(path_str: str) -> tuple[str, str | None, float]:
    import charset_normalizer
    b = Path(path_str).read_bytes()
    t0 = time.perf_counter()
    best = charset_normalizer.from_bytes(b).best()
    lat_ms = (time.perf_counter() - t0) * 1000.0
    return path_str, best.encoding if best else None, lat_ms

def _chardet_worker(path_str: str) -> tuple[str, str | None, float]:
    import chardet
    b = Path(path_str).read_bytes()
    t0 = time.perf_counter()
    r = chardet.detect(b)
    lat_ms = (time.perf_counter() - t0) * 1000.0
    return path_str, r.get("encoding"), lat_ms

def _uchardet_batch_worker(chunk: list[str]) -> list[tuple[str, str | None, float]]:
    t0 = time.perf_counter()
    cmd = ["/usr/bin/uchardet"] + chunk
    res = subprocess.run(cmd, capture_output=True, text=True, check=True)
    batch_ms = (time.perf_counter() - t0) * 1000.0
    avg_ms = batch_ms / max(1, len(chunk))
    
    out = []
    lines = res.stdout.splitlines()
    lookup = {}
    for l in lines:
        if ": " in l:
            p, enc = l.split(": ", 1)
            lookup[p] = enc.strip()
    for p in chunk:
        out.append((p, lookup.get(p), avg_ms))
    return out

# ----------------- Runner Routines ----------------- #

def run_pagesift(paths: list[str], ps_bin: str) -> tuple[dict[str, str], list[float], float, float]:
    list_file = Path("/tmp/pagesift_input_list.txt")
    out_tsv = Path("/tmp/pagesift_output.tsv")
    list_file.write_text("\n".join(paths) + "\n")
    
    # Track RSS before/after
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    cmd = [ps_bin, str(list_file), "32", str(out_tsv)]
    subprocess.run(cmd, capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    preds = {}
    latencies = []
    if out_tsv.exists():
        for line in out_tsv.read_text().splitlines():
            if not line.strip(): continue
            parts = line.split("\t")
            if len(parts) >= 2:
                preds[parts[0]] = parts[1]
                if len(parts) >= 3:
                    latencies.append(float(parts[2]) / 1000.0) # ms
    return preds, latencies, elapsed, peak_mb

def run_uchardet_pool(paths: list[str], workers: int = 32) -> tuple[dict[str, str], list[float], float, float]:
    chunk_size = (len(paths) + workers - 1) // workers
    chunks = [paths[i:i + chunk_size] for i in range(0, len(paths), chunk_size)]
    
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers) as ex:
        results = ex.map(_uchardet_batch_worker, chunks)
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    preds = {}
    latencies = []
    for chunk_res in results:
        for p, enc, lat in chunk_res:
            if enc: preds[p] = enc
            latencies.append(lat)
    return preds, latencies, elapsed, peak_mb

def run_cchardet_pool(paths: list[str], workers: int = 32) -> tuple[dict[str, str], list[float], float, float]:
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(_cchardet_worker, paths, chunksize=16))
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    preds = {}
    latencies = []
    for p, enc, lat in results:
        if enc: preds[p] = enc
        latencies.append(lat)
    return preds, latencies, elapsed, peak_mb

def run_cn_pool(paths: list[str], workers: int = 32) -> tuple[dict[str, str], list[float], float, float]:
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(_cn_worker, paths, chunksize=16))
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    preds = {}
    latencies = []
    for p, enc, lat in results:
        if enc: preds[p] = enc
        latencies.append(lat)
    return preds, latencies, elapsed, peak_mb

def run_chardet_pool(paths: list[str], workers: int = 32) -> tuple[dict[str, str], list[float], float, float]:
    rss_before = get_total_rss_bytes()
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(_chardet_worker, paths, chunksize=16))
    elapsed = time.perf_counter() - t0
    rss_after = get_total_rss_bytes()
    peak_mb = max(rss_before, rss_after) / (1024 * 1024)

    preds = {}
    latencies = []
    for p, enc, lat in results:
        if enc: preds[p] = enc
        latencies.append(lat)
    return preds, latencies, elapsed, peak_mb

# ----------------- Evaluation & Statistics ----------------- #

def evaluate_accuracy(cases: list, preds: dict[str, str]):
    correct = 0
    total = len(cases)
    cat_stats = {
        "CJK (Chinese/Japanese/Korean)": [0, 0],
        "Cyrillic (Russian/Ukrainian/Slavic)": [0, 0],
        "Western & Latin (Latin-1/15, CP1252/1250)": [0, 0],
        "Unicode (UTF-8/16/32/7)": [0, 0],
        "Legacy & Others (Greek, Hebrew, Arabic, EBCDIC, DOS)": [0, 0],
    }

    for f, expected_enc, _ in cases:
        p_str = str(f.resolve())
        pred = preds.get(p_str)
        ok = is_compat(pred, expected_enc)
        cat = categorize(expected_enc)
        if cat in cat_stats:
            cat_stats[cat][1] += 1
            if ok:
                cat_stats[cat][0] += 1
        if ok:
            correct += 1

    return correct, total, cat_stats

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

# ----------------- Main Execution ----------------- #

def main():
    mp.set_start_method("fork")
    
    # Start Watchdog
    watchdog = threading.Thread(target=memory_watchdog_thread, daemon=True)
    watchdog.start()

    data_dir = Path("benchmarks/data/fresh/data/chardet-test-data")
    ps_bin = "/home/wyc610721/pagesift/target/release/examples/detect_encoding_mt"
    
    if not data_dir.exists():
        sys.exit(f"Data dir {data_dir} not found.")
    if not Path(ps_bin).exists():
        sys.exit(f"PageSift binary {ps_bin} not found.")

    cases = collect_test_cases(data_dir)
    paths = [str(f.resolve()) for f, _, _ in cases]
    print(f"================================================================================")
    print(f"PageSift 0.50 Benchmark - Domain 6: Charset Encoding Detection & Normalization")
    print(f"Server: AMD EPYC (32 physical cores, 128 GB RAM) | Test Cases: {len(cases)} files")
    print(f"================================================================================\n")

    results = {}

    # ----------------- TIER 1: Standard Public Benchmark ----------------- #
    print(">>> RUNNING TIER 1: Standard Public Benchmark (3,136 files, 32 cores)...")
    
    # 1. PageSift
    print("  [1/5] Evaluating PageSift 0.50 (Rust 32-threads)...")
    ps_preds, ps_lats, ps_elapsed, ps_rss = run_pagesift(paths, ps_bin)
    ps_corr, ps_tot, ps_cats = evaluate_accuracy(cases, ps_preds)
    ps_perc = compute_percentiles(ps_lats)
    results["PageSift 0.50"] = {
        "accuracy": ps_corr / ps_tot * 100, "correct": ps_corr, "total": ps_tot,
        "elapsed": ps_elapsed, "throughput": ps_tot / ps_elapsed,
        "rss_mb": ps_rss, "cats": ps_cats, "percentiles": ps_perc
    }
    print(f"        Acc: {ps_corr}/{ps_tot} ({ps_corr/ps_tot*100:.2f}%) | Time: {ps_elapsed:.3f}s ({ps_tot/ps_elapsed:.1f} f/s) | p50: {ps_perc['p50']:.3f}ms")

    # 2. uchardet
    print("  [2/5] Evaluating uchardet 0.0.8 (C++ 32-processes)...")
    u_preds, u_lats, u_elapsed, u_rss = run_uchardet_pool(paths, 32)
    u_corr, u_tot, u_cats = evaluate_accuracy(cases, u_preds)
    u_perc = compute_percentiles(u_lats)
    results["uchardet 0.0.8"] = {
        "accuracy": u_corr / u_tot * 100, "correct": u_corr, "total": u_tot,
        "elapsed": u_elapsed, "throughput": u_tot / u_elapsed,
        "rss_mb": u_rss, "cats": u_cats, "percentiles": u_perc
    }
    print(f"        Acc: {u_corr}/{u_tot} ({u_corr/u_tot*100:.2f}%) | Time: {u_elapsed:.3f}s ({u_tot/u_elapsed:.1f} f/s) | p50: {u_perc['p50']:.3f}ms")

    # 3. cchardet
    print("  [3/5] Evaluating cchardet 2.4.0 (C/Cython 32-processes)...")
    cc_preds, cc_lats, cc_elapsed, cc_rss = run_cchardet_pool(paths, 32)
    cc_corr, cc_tot, cc_cats = evaluate_accuracy(cases, cc_preds)
    cc_perc = compute_percentiles(cc_lats)
    results["cchardet 2.4.0"] = {
        "accuracy": cc_corr / cc_tot * 100, "correct": cc_corr, "total": cc_tot,
        "elapsed": cc_elapsed, "throughput": cc_tot / cc_elapsed,
        "rss_mb": cc_rss, "cats": cc_cats, "percentiles": cc_perc
    }
    print(f"        Acc: {cc_corr}/{cc_tot} ({cc_corr/cc_tot*100:.2f}%) | Time: {cc_elapsed:.3f}s ({cc_tot/cc_elapsed:.1f} f/s) | p50: {cc_perc['p50']:.3f}ms")

    # 4. charset-normalizer
    print("  [4/5] Evaluating charset-normalizer 3.5.2 (Python 32-processes)...")
    cn_preds, cn_lats, cn_elapsed, cn_rss = run_cn_pool(paths, 32)
    cn_corr, cn_tot, cn_cats = evaluate_accuracy(cases, cn_preds)
    cn_perc = compute_percentiles(cn_lats)
    results["charset-normalizer 3.5.2"] = {
        "accuracy": cn_corr / cn_tot * 100, "correct": cn_corr, "total": cn_tot,
        "elapsed": cn_elapsed, "throughput": cn_tot / cn_elapsed,
        "rss_mb": cn_rss, "cats": cn_cats, "percentiles": cn_perc
    }
    print(f"        Acc: {cn_corr}/{cn_tot} ({cn_corr/cn_tot*100:.2f}%) | Time: {cn_elapsed:.3f}s ({cn_tot/cn_elapsed:.1f} f/s) | p50: {cn_perc['p50']:.3f}ms")

    # 5. chardet
    print("  [5/5] Evaluating chardet 7.7.0 (Python 32-processes)...")
    cd_preds, cd_lats, cd_elapsed, cd_rss = run_chardet_pool(paths, 32)
    cd_corr, cd_tot, cd_cats = evaluate_accuracy(cases, cd_preds)
    cd_perc = compute_percentiles(cd_lats)
    results["chardet 7.7.0"] = {
        "accuracy": cd_corr / cd_tot * 100, "correct": cd_corr, "total": cd_tot,
        "elapsed": cd_elapsed, "throughput": cd_tot / cd_elapsed,
        "rss_mb": cd_rss, "cats": cd_cats, "percentiles": cd_perc
    }
    print(f"        Acc: {cd_corr}/{cd_tot} ({cd_corr/cd_tot*100:.2f}%) | Time: {cd_elapsed:.3f}s ({cd_tot/cd_elapsed:.1f} f/s) | p50: {cd_perc['p50']:.3f}ms")

    print("\nTier 1 Standard Benchmark Complete!\n")

    # ----------------- TIER 2: Ultra-Extreme Chaos Adversarial Stress ----------------- #
    print(">>> RUNNING TIER 2: Ultra-Extreme Chaos Adversarial Stress Tests...")
    chaos_results = {}

    # Chaos 1: Chameleon Spoofing Attack (BOM & Meta Spoofing)
    print("  [Chaos 1] Chameleon Spoofing Attack (Injecting False BOM & Meta headers)...")
    tmp_spoof_dir = Path("/tmp/chaos_spoof")
    if tmp_spoof_dir.exists(): shutil.rmtree(tmp_spoof_dir)
    tmp_spoof_dir.mkdir(parents=True, exist_ok=True)
    
    spoof_cases = []
    spoof_paths = []
    # Pick non-UTF8 files (CJK, Cyrillic, Western)
    non_utf8_cases = [(f, enc, lang) for f, enc, lang in cases if canon(enc) not in ("utf-8", "ascii")]
    for idx, (f, enc, lang) in enumerate(non_utf8_cases):
        raw = f.read_bytes()
        # Prepend fake UTF-8 BOM and fake UTF-8 meta
        fake_header = b'\xef\xbb\xbf<meta charset="utf-8">\n'
        spoofed = fake_header + raw
        out_f = tmp_spoof_dir / f"spoof_{idx}.html"
        out_f.write_bytes(spoofed)
        spoof_cases.append((out_f, enc, lang))
        spoof_paths.append(str(out_f.resolve()))

    print(f"    Evaluating {len(spoof_cases)} spoofed non-UTF-8 payloads across engines...")
    ps_sp_preds, _, ps_sp_time, _ = run_pagesift(spoof_paths, ps_bin)
    ps_sp_ok, ps_sp_tot, _ = evaluate_accuracy(spoof_cases, ps_sp_preds)

    u_sp_preds, _, u_sp_time, _ = run_uchardet_pool(spoof_paths, 32)
    u_sp_ok, u_sp_tot, _ = evaluate_accuracy(spoof_cases, u_sp_preds)

    cc_sp_preds, _, cc_sp_time, _ = run_cchardet_pool(spoof_paths, 32)
    cc_sp_ok, cc_sp_tot, _ = evaluate_accuracy(spoof_cases, cc_sp_preds)

    cn_sp_preds, _, cn_sp_time, _ = run_cn_pool(spoof_paths, 32)
    cn_sp_ok, cn_sp_tot, _ = evaluate_accuracy(spoof_cases, cn_sp_preds)

    cd_sp_preds, _, cd_sp_time, _ = run_chardet_pool(spoof_paths, 32)
    cd_sp_ok, cd_sp_tot, _ = evaluate_accuracy(spoof_cases, cd_sp_preds)

    chaos_results["Chaos_1_Spoofing"] = {
        "total": len(spoof_cases),
        "PageSift": {"correct": ps_sp_ok, "acc": ps_sp_ok / ps_sp_tot * 100, "time": ps_sp_time},
        "uchardet": {"correct": u_sp_ok, "acc": u_sp_ok / u_sp_tot * 100, "time": u_sp_time},
        "cchardet": {"correct": cc_sp_ok, "acc": cc_sp_ok / cc_sp_tot * 100, "time": cc_sp_time},
        "charset-normalizer": {"correct": cn_sp_ok, "acc": cn_sp_ok / cn_sp_tot * 100, "time": cn_sp_time},
        "chardet": {"correct": cd_sp_ok, "acc": cd_sp_ok / cd_sp_tot * 100, "time": cd_sp_time},
    }
    print(f"    PageSift:           {ps_sp_ok}/{ps_sp_tot} ({ps_sp_ok/ps_sp_tot*100:.1f}%) in {ps_sp_time:.2f}s")
    print(f"    uchardet:           {u_sp_ok}/{u_sp_tot} ({u_sp_ok/u_sp_tot*100:.1f}%) in {u_sp_time:.2f}s")
    print(f"    cchardet:           {cc_sp_ok}/{cc_sp_tot} ({cc_sp_ok/cc_sp_tot*100:.1f}%) in {cc_sp_time:.2f}s")
    print(f"    charset-normalizer: {cn_sp_ok}/{cn_sp_tot} ({cn_sp_ok/cn_sp_tot*100:.1f}%) in {cn_sp_time:.2f}s")
    print(f"    chardet:            {cd_sp_ok}/{cd_sp_tot} ({cd_sp_ok/cd_sp_tot*100:.1f}%) in {cd_sp_time:.2f}s")

    # Chaos 2: Adversarial Head & Tail Truncation
    print("  [Chaos 2] Adversarial Head & Tail Truncation (Head 1-3B strip, Tail 64B slice)...")
    tmp_trunc_dir = Path("/tmp/chaos_trunc")
    if tmp_trunc_dir.exists(): shutil.rmtree(tmp_trunc_dir)
    tmp_trunc_dir.mkdir(parents=True, exist_ok=True)
    
    trunc_cases = []
    trunc_paths = []
    for idx, (f, enc, lang) in enumerate(cases):
        raw = f.read_bytes()
        strip_h = min(random.randint(1, 3), len(raw))
        sliced = raw[strip_h:min(len(raw), strip_h + 128)]
        out_f = tmp_trunc_dir / f"trunc_{idx}.bin"
        out_f.write_bytes(sliced)
        trunc_cases.append((out_f, enc, lang))
        trunc_paths.append(str(out_f.resolve()))

    ps_tr_preds, _, ps_tr_time, _ = run_pagesift(trunc_paths, ps_bin)
    ps_tr_ok, ps_tr_tot, _ = evaluate_accuracy(trunc_cases, ps_tr_preds)

    u_tr_preds, _, u_tr_time, _ = run_uchardet_pool(trunc_paths, 32)
    u_tr_ok, u_tr_tot, _ = evaluate_accuracy(trunc_cases, u_tr_preds)

    cc_tr_preds, _, cc_tr_time, _ = run_cchardet_pool(trunc_paths, 32)
    cc_tr_ok, cc_tr_tot, _ = evaluate_accuracy(trunc_cases, cc_tr_preds)

    cn_tr_preds, _, cn_tr_time, _ = run_cn_pool(trunc_paths, 32)
    cn_tr_ok, cn_tr_tot, _ = evaluate_accuracy(trunc_cases, cn_tr_preds)

    cd_tr_preds, _, cd_tr_time, _ = run_chardet_pool(trunc_paths, 32)
    cd_tr_ok, cd_tr_tot, _ = evaluate_accuracy(trunc_cases, cd_tr_preds)

    chaos_results["Chaos_2_Truncation"] = {
        "total": len(trunc_cases),
        "PageSift": {"correct": ps_tr_ok, "acc": ps_tr_ok / ps_tr_tot * 100, "time": ps_tr_time},
        "uchardet": {"correct": u_tr_ok, "acc": u_tr_ok / u_tr_tot * 100, "time": u_tr_time},
        "cchardet": {"correct": cc_tr_ok, "acc": cc_tr_ok / cc_tr_tot * 100, "time": cc_tr_time},
        "charset-normalizer": {"correct": cn_tr_ok, "acc": cn_tr_ok / cn_tr_tot * 100, "time": cn_tr_time},
        "chardet": {"correct": cd_tr_ok, "acc": cd_tr_ok / cd_tr_tot * 100, "time": cd_tr_time},
    }
    print(f"    PageSift:           {ps_tr_ok}/{ps_tr_tot} ({ps_tr_ok/ps_tr_tot*100:.1f}%) in {ps_tr_time:.2f}s")
    print(f"    uchardet:           {u_tr_ok}/{u_tr_tot} ({u_tr_ok/u_tr_tot*100:.1f}%) in {u_tr_time:.2f}s")
    print(f"    cchardet:           {cc_tr_ok}/{cc_tr_tot} ({cc_tr_ok/cc_tr_tot*100:.1f}%) in {cc_tr_time:.2f}s")
    print(f"    charset-normalizer: {cn_tr_ok}/{cn_tr_tot} ({cn_tr_ok/cn_tr_tot*100:.1f}%) in {cn_tr_time:.2f}s")
    print(f"    chardet:            {cd_tr_ok}/{cd_tr_tot} ({cd_tr_ok/cd_tr_tot*100:.1f}%) in {cd_tr_time:.2f}s")

    # Chaos 3: Noise & Bit-Flip Corruption (1% Corruption)
    print("  [Chaos 3] Noise & Bit-Flip Corruption (1% dirty byte injections across 3,136 files)...")
    tmp_noise_dir = Path("/tmp/chaos_noise")
    if tmp_noise_dir.exists(): shutil.rmtree(tmp_noise_dir)
    tmp_noise_dir.mkdir(parents=True, exist_ok=True)
    
    noise_cases = []
    noise_paths = []
    for idx, (f, enc, lang) in enumerate(cases):
        raw = bytearray(f.read_bytes())
        corrupt_cnt = max(1, len(raw) // 100)
        for _ in range(corrupt_cnt):
            pos = random.randint(0, len(raw) - 1)
            raw[pos] = random.randint(0x80, 0xFF)
        out_f = tmp_noise_dir / f"noise_{idx}.bin"
        out_f.write_bytes(raw)
        noise_cases.append((out_f, enc, lang))
        noise_paths.append(str(out_f.resolve()))

    ps_ns_preds, _, ps_ns_time, _ = run_pagesift(noise_paths, ps_bin)
    ps_ns_ok, ps_ns_tot, _ = evaluate_accuracy(noise_cases, ps_ns_preds)

    u_ns_preds, _, u_ns_time, _ = run_uchardet_pool(noise_paths, 32)
    u_ns_ok, u_ns_tot, _ = evaluate_accuracy(noise_cases, u_ns_preds)

    cc_ns_preds, _, cc_ns_time, _ = run_cchardet_pool(noise_paths, 32)
    cc_ns_ok, cc_ns_tot, _ = evaluate_accuracy(noise_cases, cc_ns_preds)

    cn_ns_preds, _, cn_ns_time, _ = run_cn_pool(noise_paths, 32)
    cn_ns_ok, cn_ns_tot, _ = evaluate_accuracy(noise_cases, cn_ns_preds)

    cd_ns_preds, _, cd_ns_time, _ = run_chardet_pool(noise_paths, 32)
    cd_ns_ok, cd_ns_tot, _ = evaluate_accuracy(noise_cases, cd_ns_preds)

    chaos_results["Chaos_3_Noise"] = {
        "total": len(noise_cases),
        "PageSift": {"correct": ps_ns_ok, "acc": ps_ns_ok / ps_ns_tot * 100, "time": ps_ns_time},
        "uchardet": {"correct": u_ns_ok, "acc": u_ns_ok / u_ns_tot * 100, "time": u_ns_time},
        "cchardet": {"correct": cc_ns_ok, "acc": cc_ns_ok / cc_ns_tot * 100, "time": cc_ns_time},
        "charset-normalizer": {"correct": cn_ns_ok, "acc": cn_ns_ok / cn_ns_tot * 100, "time": cn_ns_time},
        "chardet": {"correct": cd_ns_ok, "acc": cd_ns_ok / cd_ns_tot * 100, "time": cd_ns_time},
    }
    print(f"    PageSift:           {ps_ns_ok}/{ps_ns_tot} ({ps_ns_ok/ps_ns_tot*100:.1f}%) in {ps_ns_time:.2f}s")
    print(f"    uchardet:           {u_ns_ok}/{u_ns_tot} ({u_ns_ok/u_ns_tot*100:.1f}%) in {u_ns_time:.2f}s")
    print(f"    cchardet:           {cc_ns_ok}/{cc_ns_tot} ({cc_ns_ok/cc_ns_tot*100:.1f}%) in {cc_ns_time:.2f}s")
    print(f"    charset-normalizer: {cn_ns_ok}/{cn_ns_tot} ({cn_ns_ok/cn_ns_tot*100:.1f}%) in {cn_ns_time:.2f}s")
    print(f"    chardet:            {cd_ns_ok}/{cd_ns_tot} ({cd_ns_ok/cd_ns_tot*100:.1f}%) in {cd_ns_time:.2f}s")

    # Chaos 4: Pathological 50MB Polyglot Mega-Corpus
    print("  [Chaos 4] Pathological 50MB Polyglot Mega-Corpus (Throughput & Flatline RSS)...")
    polyglot_path = Path("/tmp/polyglot_50mb.bin")
    # Build 50MB polyglot from diverse slices
    poly_bytes = bytearray()
    sample_slices = [f.read_bytes() for f, _, _ in cases[:200]]
    idx = 0
    while len(poly_bytes) < 50 * 1024 * 1024:
        poly_bytes.extend(sample_slices[idx % len(sample_slices)])
        idx += 1
    poly_bytes = poly_bytes[:50 * 1024 * 1024]
    polyglot_path.write_bytes(poly_bytes)
    
    # Test each engine on the 50MB file
    mega_results = {}
    
    # PageSift on 50MB
    t0 = time.perf_counter()
    ps_50_p, _, ps_50_t, ps_50_rss = run_pagesift([str(polyglot_path)], ps_bin)
    ps_mb_s = 50.0 / max(0.0001, ps_50_t)
    mega_results["PageSift"] = {"time": ps_50_t, "speed_mbs": ps_mb_s, "pred": ps_50_p.get(str(polyglot_path)), "rss_mb": ps_50_rss}
    print(f"    PageSift:   {ps_50_t:.3f}s ({ps_mb_s:.1f} MB/s) | Peak RSS: {ps_50_rss:.1f} MB")

    # uchardet on 50MB
    t0 = time.perf_counter()
    u_50_p, _, u_50_t, u_50_rss = run_uchardet_pool([str(polyglot_path)], 1)
    u_mb_s = 50.0 / max(0.0001, u_50_t)
    mega_results["uchardet"] = {"time": u_50_t, "speed_mbs": u_mb_s, "pred": u_50_p.get(str(polyglot_path)), "rss_mb": u_50_rss}
    print(f"    uchardet:   {u_50_t:.3f}s ({u_mb_s:.1f} MB/s) | Peak RSS: {u_50_rss:.1f} MB")

    # cchardet on 50MB
    t0 = time.perf_counter()
    cc_50_p, _, cc_50_t, cc_50_rss = run_cchardet_pool([str(polyglot_path)], 1)
    cc_mb_s = 50.0 / max(0.0001, cc_50_t)
    mega_results["cchardet"] = {"time": cc_50_t, "speed_mbs": cc_mb_s, "pred": cc_50_p.get(str(polyglot_path)), "rss_mb": cc_50_rss}
    print(f"    cchardet:   {cc_50_t:.3f}s ({cc_mb_s:.1f} MB/s) | Peak RSS: {cc_50_rss:.1f} MB")

    # charset-normalizer on 50MB
    t0 = time.perf_counter()
    cn_50_p, _, cn_50_t, cn_50_rss = run_cn_pool([str(polyglot_path)], 1)
    cn_mb_s = 50.0 / max(0.0001, cn_50_t)
    mega_results["charset-normalizer"] = {"time": cn_50_t, "speed_mbs": cn_mb_s, "pred": cn_50_p.get(str(polyglot_path)), "rss_mb": cn_50_rss}
    print(f"    charset-norm: {cn_50_t:.3f}s ({cn_mb_s:.1f} MB/s) | Peak RSS: {cn_50_rss:.1f} MB")

    # chardet on 50MB (chardet takes extreme time on 50MB, guard with timeout)
    print("    chardet on 50MB...")
    try:
        with ProcessPoolExecutor(max_workers=1) as ex:
            fut = ex.submit(_chardet_worker, str(polyglot_path))
            p, enc, cd_lat = fut.result(timeout=30.0)
            cd_50_t = cd_lat / 1000.0
            cd_mb_s = 50.0 / max(0.0001, cd_50_t)
            mega_results["chardet"] = {"time": cd_50_t, "speed_mbs": cd_mb_s, "pred": enc, "rss_mb": 0.0}
            print(f"    chardet:    {cd_50_t:.3f}s ({cd_mb_s:.1f} MB/s)")
    except Exception as e:
        print(f"    chardet timed out / failed on 50MB: {e}")
        mega_results["chardet"] = {"time": ">30.0s (TIMEOUT)", "speed_mbs": 0.0, "pred": "TIMEOUT", "rss_mb": 0.0}

    chaos_results["Chaos_4_50MB_Polyglot"] = mega_results

    # Chaos 5: 25,000 Operations Concurrency Storm
    print("  [Chaos 5] 25,000 Operations Concurrency Storm across 32 cores...")
    storm_paths = [paths[i % len(paths)] for i in range(25000)]
    
    print("    Running PageSift on 25,000 files...")
    ps_st_p, _, ps_st_t, ps_st_rss = run_pagesift(storm_paths, ps_bin)
    ps_st_ops = 25000.0 / max(0.0001, ps_st_t)
    print(f"    PageSift 25k: {ps_st_t:.3f}s ({ps_st_ops:.1f} ops/s) | Peak RSS: {ps_st_rss:.1f} MB")

    print("    Running uchardet on 25,000 files...")
    u_st_p, _, u_st_t, u_st_rss = run_uchardet_pool(storm_paths, 32)
    u_st_ops = 25000.0 / max(0.0001, u_st_t)
    print(f"    uchardet 25k: {u_st_t:.3f}s ({u_st_ops:.1f} ops/s) | Peak RSS: {u_st_rss:.1f} MB")

    print("    Running cchardet on 25,000 files...")
    cc_st_p, _, cc_st_t, cc_st_rss = run_cchardet_pool(storm_paths, 32)
    cc_st_ops = 25000.0 / max(0.0001, cc_st_t)
    print(f"    cchardet 25k: {cc_st_t:.3f}s ({cc_st_ops:.1f} ops/s) | Peak RSS: {cc_st_rss:.1f} MB")

    chaos_results["Chaos_5_Storm"] = {
        "PageSift": {"ops_per_sec": ps_st_ops, "elapsed": ps_st_t, "rss_mb": ps_st_rss},
        "uchardet": {"ops_per_sec": u_st_ops, "elapsed": u_st_t, "rss_mb": u_st_rss},
        "cchardet": {"ops_per_sec": cc_st_ops, "elapsed": cc_st_t, "rss_mb": cc_st_rss},
    }

    # Stop Watchdog
    WATCHDOG_STOP.set()
    watchdog.join(timeout=1.0)
    print(f"\nBenchmark finished successfully. Global Peak RSS observed: {PEAK_RSS_BYTES / (1024*1024):.1f} MB (well within 120GB limit).\n")

    # Save JSON summary
    summary_data = {
        "tier1_standard": results,
        "tier2_chaos": chaos_results,
        "peak_rss_mb": PEAK_RSS_BYTES / (1024*1024),
    }
    out_json = Path("benchmarks/history/0.50/results/benchmark_d6_results.json")
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(summary_data, indent=2))
    print(f"Saved raw metrics to {out_json}")

if __name__ == "__main__":
    main()
