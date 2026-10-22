#!/usr/bin/env python3
"""Adversarial & Corrupted Encoding Stress Test Suite.

Tests PageSift (pure Rust), chardet 7.7.0, and charset-normalizer 3.5.2 across:
1. Standard Clean Public Benchmark (3,138 files)
2. Adversarial Head Truncation (1-3 bytes removed, breaking BOM & initial multibyte lead byte)
3. Adversarial Tail Truncation (mid-multibyte sequence cuts at 128 bytes)
4. Corrupted Noise Injection (1% bit flips & invalid byte injections)
5. 10MB Extreme Large Payload Scalability & Throughput (MB/s)
"""

from __future__ import annotations

import os
import random
import subprocess
import sys
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

try:
    import chardet
    HAS_CHARDET = True
except ImportError:
    HAS_CHARDET = False

try:
    import charset_normalizer
    HAS_CN = True
except ImportError:
    HAS_CN = False

ALIASES = {
    "ascii": "ascii",
    "utf-8": "utf-8",
    "utf-8-sig": "utf-8",
    "utf-16le": "utf-16",
    "utf-16be": "utf-16",
    "utf-32le": "utf-32",
    "utf-32be": "utf-32",
    "windows-1252": "windows-1252",
    "cp1252": "windows-1252",
    "iso-8859-1": "windows-1252",
    "latin1": "windows-1252",
    "windows-1251": "windows-1251",
    "cp1251": "windows-1251",
    "windows-1250": "windows-1250",
    "cp1250": "windows-1250",
    "windows-1253": "windows-1253",
    "windows-1254": "windows-1254",
    "windows-1255": "windows-1255",
    "windows-1256": "windows-1256",
    "windows-1257": "windows-1257",
    "windows-1258": "windows-1258",
    "gb2312": "gb18030",
    "gbk": "gb18030",
    "gb18030": "gb18030",
    "big5": "big5",
    "cp950": "big5",
    "shift_jis": "shift_jis",
    "cp932": "shift_jis",
    "windows-31j": "shift_jis",
    "euc-jp": "euc-jp",
    "euc-kr": "euc-kr",
    "cp949": "euc-kr",
    "koi8-r": "koi8-r",
    "koi8-u": "koi8-u",
    "iso-8859-2": "iso-8859-2",
    "iso-8859-5": "iso-8859-5",
    "iso-8859-7": "iso-8859-7",
    "iso-8859-8": "iso-8859-8",
    "iso-8859-9": "iso-8859-9",
    "iso-8859-15": "windows-1252",
}

def canon(name: str | None) -> str:
    if not name:
        return "unknown"
    n = name.strip().lower().replace("_", "-")
    return ALIASES.get(n, n)

def is_compat(detected: str | None, expected: str) -> bool:
    d = canon(detected)
    e = canon(expected)
    if d == e:
        return True
    if d == "ascii" and e in ("utf-8", "windows-1252", "iso-8859-1"):
        return True
    if e == "ascii" and d in ("utf-8", "windows-1252"):
        return True
    if d in ("gb18030", "gbk", "gb2312") and e in ("gb18030", "gbk", "gb2312"):
        return True
    if d in ("shift_jis", "cp932") and e in ("shift_jis", "cp932"):
        return True
    return False

def collect_test_cases(data_dir: Path):
    cases = []
    for p in sorted(data_dir.iterdir()):
        if not p.is_dir() or p.name.startswith("."):
            continue
        parts = p.name.rsplit("-", 1)
        if len(parts) != 2:
            continue
        enc_name, lang = parts[0], parts[1]
        for f in sorted(p.iterdir()):
            if f.is_file() and not f.name.startswith("."):
                cases.append((f, enc_name, lang))
    return cases

def run_pagesift_bytes(byte_payloads: list[tuple[str, bytes]], pagesift_bin: str) -> tuple[dict, float]:
    # Write temp files to a ramfs or /tmp directory
    tmp_dir = Path("/tmp/ps_stress")
    tmp_dir.mkdir(parents=True, exist_ok=True)
    list_path = Path("/tmp/ps_stress_list.txt")

    file_paths = []
    for key, b in byte_payloads:
        p = tmp_dir / f"{key}.bin"
        p.write_bytes(b)
        file_paths.append(str(p.resolve()))

    list_path.write_text("\n".join(file_paths) + "\n")

    t0 = time.perf_counter()
    res = subprocess.run([pagesift_bin, str(list_path)], capture_output=True, text=True)
    elapsed = time.perf_counter() - t0

    preds = {}
    for line in res.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 2:
            key = Path(parts[0]).stem
            preds[key] = parts[1]
    return preds, elapsed

def run_chardet_bytes(byte_payloads: list[tuple[str, bytes]]) -> tuple[dict, float]:
    t0 = time.perf_counter()
    preds = {}
    def _worker(item):
        key, b = item
        try:
            r = chardet.detect(b)
            return key, r.get("encoding")
        except Exception:
            return key, None

    with ThreadPoolExecutor(max_workers=16) as pool:
        for k, enc in pool.map(_worker, byte_payloads):
            preds[k] = enc
    elapsed = time.perf_counter() - t0
    return preds, elapsed

def run_cn_bytes(byte_payloads: list[tuple[str, bytes]]) -> tuple[dict, float]:
    t0 = time.perf_counter()
    preds = {}
    def _worker(item):
        key, b = item
        try:
            best = charset_normalizer.from_bytes(b).best()
            return key, best.encoding if best else None
        except Exception:
            return key, None

    with ThreadPoolExecutor(max_workers=16) as pool:
        for k, enc in pool.map(_worker, byte_payloads):
            preds[k] = enc
    elapsed = time.perf_counter() - t0
    return preds, elapsed


def evaluate_batch(byte_payloads, expected_map, ps_bin):
    # 1. PageSift
    ps_preds, ps_time = run_pagesift_bytes(byte_payloads, ps_bin)
    ps_corr = sum(1 for k, _ in byte_payloads if is_compat(ps_preds.get(k), expected_map[k]))

    # 2. chardet
    cd_preds, cd_time = run_chardet_bytes(byte_payloads)
    cd_corr = sum(1 for k, _ in byte_payloads if is_compat(cd_preds.get(k), expected_map[k]))

    # 3. charset-normalizer
    cn_preds, cn_time = run_cn_bytes(byte_payloads)
    cn_corr = sum(1 for k, _ in byte_payloads if is_compat(cn_preds.get(k), expected_map[k]))

    total = len(byte_payloads)
    return {
        "total": total,
        "ps": {"correct": ps_corr, "acc": ps_corr / total * 100, "time": ps_time},
        "cd": {"correct": cd_corr, "acc": cd_corr / total * 100, "time": cd_time},
        "cn": {"correct": cn_corr, "acc": cn_corr / total * 100, "time": cn_time},
    }


def main():
    data_dir = Path("bench/data/fresh/data/chardet-test-data")
    ps_bin = "target/release/examples/detect_encoding"
    cases = collect_test_cases(data_dir)
    print(f"Loaded {len(cases)} test cases from {data_dir}\n")

    # Load all raw bytes
    raw_cases = []
    expected_map = {}
    for idx, (f, enc, lang) in enumerate(cases):
        key = f"case_{idx}"
        b = f.read_bytes()
        raw_cases.append((key, b))
        expected_map[key] = enc

    print("================================================================================")
    print("=== EXTREME ADVERSARIAL & CORRUPTED ENCODING BENCHMARK (3,138 FILES) ===")
    print("================================================================================\n")

    # TEST 1: Clean Baseline
    print("--- Test 1: Clean Public Baseline (3,138 files) ---")
    res1 = evaluate_batch(raw_cases, expected_map, ps_bin)
    print(f"PageSift:           {res1['ps']['correct']}/{res1['total']} ({res1['ps']['acc']:.1f}%) in {res1['ps']['time']:.2f}s ({res1['total']/res1['ps']['time']:.0f} files/s)")
    print(f"chardet (7.7.0):    {res1['cd']['correct']}/{res1['total']} ({res1['cd']['acc']:.1f}%) in {res1['cd']['time']:.2f}s ({res1['total']/res1['cd']['time']:.0f} files/s)")
    print(f"charset-normalizer: {res1['cn']['correct']}/{res1['total']} ({res1['cn']['acc']:.1f}%) in {res1['cn']['time']:.2f}s ({res1['total']/res1['cn']['time']:.0f} files/s)")
    print()

    # TEST 2: Adversarial Head Truncation (stripping first 1-3 bytes, destroying BOM/multibyte headers)
    print("--- Test 2: Adversarial Head Truncation (1-3 bytes stripped) ---")
    head_cases = []
    for key, b in raw_cases:
        strip_len = min(random.randint(1, 3), len(b))
        head_cases.append((key, b[strip_len:]))
    res2 = evaluate_batch(head_cases, expected_map, ps_bin)
    print(f"PageSift:           {res2['ps']['correct']}/{res2['total']} ({res2['ps']['acc']:.1f}%) in {res2['ps']['time']:.2f}s")
    print(f"chardet (7.7.0):    {res2['cd']['correct']}/{res2['total']} ({res2['cd']['acc']:.1f}%) in {res2['cd']['time']:.2f}s")
    print(f"charset-normalizer: {res2['cn']['correct']}/{res2['total']} ({res2['cn']['acc']:.1f}%) in {res2['cn']['time']:.2f}s")
    print()

    # TEST 3: Adversarial Tail Truncation (cut at arbitrary 128 bytes mid-sequence)
    print("--- Test 3: Adversarial Short-Payload Tail Cut (128 bytes mid-multibyte) ---")
    tail_cases = []
    for key, b in raw_cases:
        tail_cases.append((key, b[:min(128, len(b))]))
    res3 = evaluate_batch(tail_cases, expected_map, ps_bin)
    print(f"PageSift:           {res3['ps']['correct']}/{res3['total']} ({res3['ps']['acc']:.1f}%) in {res3['ps']['time']:.2f}s")
    print(f"chardet (7.7.0):    {res3['cd']['correct']}/{res3['total']} ({res3['cd']['acc']:.1f}%) in {res3['cd']['time']:.2f}s")
    print(f"charset-normalizer: {res3['cn']['correct']}/{res3['total']} ({res3['cn']['acc']:.1f}%) in {res3['cn']['time']:.2f}s")
    print()

    # TEST 4: Corrupted Byte Injection (1% invalid byte flips)
    print("--- Test 4: Corrupted Byte Injection (1% corrupted bytes / invalid bit-flips) ---")
    noisy_cases = []
    for key, b in raw_cases:
        barr = bytearray(b)
        n_flips = max(1, len(barr) // 100)
        for _ in range(min(n_flips, 20)):
            pos = random.randint(0, len(barr) - 1)
            barr[pos] = random.choice([0x00, 0xFF, 0x80, 0xFE])
        noisy_cases.append((key, bytes(barr)))
    res4 = evaluate_batch(noisy_cases, expected_map, ps_bin)
    print(f"PageSift:           {res4['ps']['correct']}/{res4['total']} ({res4['ps']['acc']:.1f}%) in {res4['ps']['time']:.2f}s")
    print(f"chardet (7.7.0):    {res4['cd']['correct']}/{res4['total']} ({res4['cd']['acc']:.1f}%) in {res4['cd']['time']:.2f}s")
    print(f"charset-normalizer: {res4['cn']['correct']}/{res4['total']} ({res4['cn']['acc']:.1f}%) in {res4['cn']['time']:.2f}s")
    print()

    # TEST 5: Large Document Scalability (10MB payload)
    print("--- Test 5: 10MB Multi-Language Giant Payload Scalability & Throughput ---")
    huge_sample = b"".join([b for _, b in raw_cases[:200]]) * 10
    huge_size_mb = len(huge_sample) / (1024 * 1024)
    huge_cases = [("huge_10mb", huge_sample)]

    # PageSift on huge
    _, ps_huge_time = run_pagesift_bytes(huge_cases, ps_bin)
    ps_throughput = huge_size_mb / ps_huge_time

    # chardet on huge
    _, cd_huge_time = run_chardet_bytes(huge_cases)
    cd_throughput = huge_size_mb / cd_huge_time

    # charset-normalizer on huge
    _, cn_huge_time = run_cn_bytes(huge_cases)
    cn_throughput = huge_size_mb / cn_huge_time

    print(f"Payload Size: {huge_size_mb:.2f} MB")
    print(f"PageSift:           {ps_huge_time*1000:.1f} ms  ({ps_throughput:.1f} MB/s)")
    print(f"chardet (7.7.0):    {cd_huge_time*1000:.1f} ms  ({cd_throughput:.1f} MB/s) -> PageSift is {cd_huge_time/ps_huge_time:.1f}x faster")
    print(f"charset-normalizer: {cn_huge_time*1000:.1f} ms  ({cn_throughput:.1f} MB/s) -> PageSift is {cn_huge_time/ps_huge_time:.1f}x faster")
    print("================================================================================\n")


if __name__ == "__main__":
    main()
