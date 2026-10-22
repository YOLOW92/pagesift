#!/usr/bin/env python3
"""Encoding Detection Benchmark Driver on chardet-test-data (3,161 files).

Compares:
1. PageSift (pure Rust, chardetng/BOM)
2. chardet
3. charset-normalizer
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

# Mapping and normalization of encoding names to canonical families
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
    "iso-8859-1": "windows-1252",  # Web standard treats iso-8859-1 as windows-1252
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
    "big5hkscs": "big5",
    "cp950": "big5",
    "shift_jis": "shift_jis",
    "sjis": "shift_jis",
    "cp932": "shift_jis",
    "windows-31j": "shift_jis",
    "euc-jp": "euc-jp",
    "euc_jp": "euc-jp",
    "euc-kr": "euc-kr",
    "euc_kr": "euc-kr",
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


def is_compat(detected: str, expected: str) -> bool:
    d = canon(detected)
    e = canon(expected)
    if d == e:
        return True
    # ASCII is valid subset of UTF-8, Windows-1252, etc.
    if d == "ascii" and e in ("utf-8", "windows-1252", "iso-8859-1"):
        return True
    if e == "ascii" and d in ("utf-8", "windows-1252"):
        return True
    # GB family compatibility
    if d in ("gb18030", "gbk", "gb2312") and e in ("gb18030", "gbk", "gb2312"):
        return True
    # Shift-JIS / CP932 compatibility
    if d in ("shift_jis", "cp932") and e in ("shift_jis", "cp932"):
        return True
    return False


def collect_test_cases(data_dir: Path):
    cases = []
    for p in sorted(data_dir.iterdir()):
        if not p.is_dir() or p.name.startswith("."):
            continue
        # Folder format is <encoding>-<lang>
        parts = p.name.rsplit("-", 1)
        if len(parts) != 2:
            continue
        enc_name, lang = parts[0], parts[1]
        for f in sorted(p.iterdir()):
            if f.is_file() and not f.name.startswith("."):
                cases.append((f, enc_name, lang))
    return cases


def run_pagesift(cases: list, pagesift_bin: str):
    list_file = Path("/tmp/pagesift_enc_list.txt")
    with list_file.open("w") as out:
        for f, _, _ in cases:
            out.write(f"{f.resolve()}\n")

    t0 = time.perf_counter()
    res = subprocess.run([pagesift_bin, str(list_file)], capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - t0

    preds = {}
    latencies = []
    for line in res.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 2:
            preds[parts[0]] = parts[1]
            if len(parts) >= 3:
                latencies.append(float(parts[2]) / 1000.0)  # ms
    return preds, latencies, elapsed


from concurrent.futures import ThreadPoolExecutor

def run_chardet(cases: list, py_bin: str):
    import chardet
    print(f"  (using chardet {chardet.__version__})")

    t0 = time.perf_counter()
    preds = {}
    latencies = []

    def _worker(item):
        f, _, _ = item
        b = f.read_bytes()
        t_start = time.perf_counter()
        r = chardet.detect(b)
        lat = (time.perf_counter() - t_start) * 1000.0
        return str(f.resolve()), r.get("encoding"), lat

    with ThreadPoolExecutor(max_workers=16) as pool:
        for path_str, enc, lat in pool.map(_worker, cases):
            preds[path_str] = enc
            latencies.append(lat)

    elapsed = time.perf_counter() - t0
    return preds, latencies, elapsed


def run_charset_normalizer(cases: list):
    from charset_normalizer import from_bytes
    import charset_normalizer
    print(f"  (using charset-normalizer {charset_normalizer.__version__})")

    t0 = time.perf_counter()
    preds = {}
    latencies = []

    def _worker(item):
        f, _, _ = item
        b = f.read_bytes()
        t_start = time.perf_counter()
        best = from_bytes(b).best()
        lat = (time.perf_counter() - t_start) * 1000.0
        return str(f.resolve()), best.encoding if best else None, lat

    with ThreadPoolExecutor(max_workers=16) as pool:
        for path_str, enc, lat in pool.map(_worker, cases):
            preds[path_str] = enc
            latencies.append(lat)

    elapsed = time.perf_counter() - t0
    return preds, latencies, elapsed



def evaluate_preds(cases: list, preds: dict):
    correct = 0
    total = len(cases)
    by_category = {
        "CJK (Chinese/Japanese/Korean)": [0, 0],
        "Cyrillic (Russian/Ukrainian)": [0, 0],
        "Western (Latin/Windows-1252)": [0, 0],
        "Unicode (UTF-8/UTF-16)": [0, 0],
        "Legacy/Others (EBCDIC/CP437)": [0, 0],
    }

    for f, expected_enc, lang in cases:
        p = preds.get(str(f.resolve()))
        ok = is_compat(p, expected_enc)

        # Categorize
        e_canon = canon(expected_enc)
        if e_canon in ("gb18030", "big5", "shift_jis", "euc-jp", "euc-kr"):
            cat = "CJK (Chinese/Japanese/Korean)"
        elif e_canon in ("windows-1251", "koi8-r", "koi8-u", "iso-8859-5"):
            cat = "Cyrillic (Russian/Ukrainian)"
        elif e_canon in ("windows-1252", "iso-8859-1", "iso-8859-2", "windows-1250"):
            cat = "Western (Latin/Windows-1252)"
        elif e_canon in ("utf-8", "utf-16", "utf-32"):
            cat = "Unicode (UTF-8/UTF-16)"
        else:
            cat = "Legacy/Others (EBCDIC/CP437)"

        by_category[cat][1] += 1
        if ok:
            correct += 1
            by_category[cat][0] += 1

    return correct, total, by_category


def main():
    data_dir = Path("bench/data/fresh/data/chardet-test-data")
    if not data_dir.exists():
        sys.exit(f"Data dir {data_dir} does not exist.")

    cases = collect_test_cases(data_dir)
    print(f"Loaded {len(cases)} test cases from {data_dir}\n")

    pagesift_bin = "target/release/examples/detect_encoding"
    if not Path(pagesift_bin).exists():
        sys.exit(f"PageSift binary {pagesift_bin} not found. Run cargo build --release -p pagesift --example detect_encoding")

    # 1. Evaluate PageSift
    print("Evaluating PageSift (Rust)...")
    ps_preds, ps_latencies, ps_elapsed = run_pagesift(cases, pagesift_bin)
    ps_ok, total, ps_cats = evaluate_preds(cases, ps_preds)

    # 2. Evaluate chardet (Python)
    print("Evaluating chardet...")
    cd_preds, cd_latencies, cd_elapsed = run_chardet(cases, sys.executable)
    cd_ok, _, cd_cats = evaluate_preds(cases, cd_preds)

    # 3. Evaluate charset-normalizer (Python)
    print("Evaluating charset-normalizer...")
    cn_preds, cn_latencies, cn_elapsed = run_charset_normalizer(cases)
    cn_ok, _, cn_cats = evaluate_preds(cases, cn_preds)

    # Summary Table
    print("\n" + "=" * 70)
    print(f"{'Tool':<22} {'Accuracy (All)':<16} {'Avg Latency':<16} {'Total Time':<12}")
    print("=" * 70)
    print(f"{'PageSift (Current)':<22} {ps_ok}/{total} ({ps_ok/total*100:.1f}%)   {sum(ps_latencies)/len(ps_latencies):.2f} ms/file    {ps_elapsed:.2f} s")
    print(f"{'chardet (5.2.0)':<22} {cd_ok}/{total} ({cd_ok/total*100:.1f}%)   {sum(cd_latencies)/len(cd_latencies):.2f} ms/file   {cd_elapsed:.2f} s")
    print(f"{'charset-normalizer':<22} {cn_ok}/{total} ({cn_ok/total*100:.1f}%)   {sum(cn_latencies)/len(cn_latencies):.2f} ms/file   {cn_elapsed:.2f} s")
    print("=" * 70)

    print("\nAccuracy Breakdown by Category:")
    print(f"{'Category':<32} {'PageSift':<14} {'chardet':<14} {'charset-normalizer':<14}")
    print("-" * 74)
    for cat in ps_cats:
        ps_c, ps_t = ps_cats[cat]
        cd_c, _ = cd_cats[cat]
        cn_c, _ = cn_cats[cat]
        ps_str = f"{ps_c}/{ps_t} ({ps_c/ps_t*100:.1f}%)" if ps_t else "N/A"
        cd_str = f"{cd_c}/{ps_t} ({cd_c/ps_t*100:.1f}%)" if ps_t else "N/A"
        cn_str = f"{cn_c}/{ps_t} ({cn_c/ps_t*100:.1f}%)" if ps_t else "N/A"
        print(f"{cat:<32} {ps_str:<14} {cd_str:<14} {cn_str:<14}")


if __name__ == "__main__":
    main()
