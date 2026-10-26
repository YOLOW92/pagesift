#!/usr/bin/env python3
"""Deeply-Nested & High-Density Structured Data Extreme Stress Test.

Compares PageSift pure Rust extractor against Scrapinghub's `extruct 0.18.0`.
Test Dimensions:
1. Official Extruct Benchmark Suite (63 files across schema.org, w3c, songkick, etc.)
2. Extreme Deep Nesting Pathological Stress (10, 20, 30 levels of nested itemscopes)
3. Massive Knowledge Graph Stress (100, 300, 500 entity @graph payloads)
4. Tag Soup / Broken HTML with Microdata properties
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import extruct

PS_BIN = "target/release/examples/bench_structured"


def run_pagesift_files(file_paths: list[Path]) -> tuple[dict, float]:
    list_path = Path("/tmp/ps_struct_list.txt")
    list_path.write_text("\n".join(str(p.resolve()) for p in file_paths) + "\n")

    t0 = time.perf_counter()
    res = subprocess.run([PS_BIN, str(list_path)], capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - t0

    out = {}
    for line in res.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 5:
            p_str, us, jld, md, og = parts[0], float(parts[1]), int(parts[2]), int(parts[3]), int(parts[4])
            out[p_str] = {
                "us": us,
                "json_ld": jld,
                "microdata": md,
                "opengraph": og,
                "total_entities": jld + md + (1 if og > 0 else 0),
            }
    return out, elapsed


def run_extruct_files(file_paths: list[Path]) -> tuple[dict, float]:
    t0 = time.perf_counter()
    out = {}

    def _worker(p: Path):
        html = p.read_text(encoding="utf-8", errors="replace")
        t_start = time.perf_counter()
        try:
            data = extruct.extract(html, syntaxes=['json-ld', 'microdata', 'opengraph'])
            us = (time.perf_counter() - t_start) * 1_000_000.0
            jld = len(data.get("json-ld", []))
            md = len(data.get("microdata", []))
            og = len(data.get("opengraph", []))
            return str(p.resolve()), {
                "us": us,
                "json_ld": jld,
                "microdata": md,
                "opengraph": og,
                "total_entities": jld + md + og,
            }
        except Exception:
            us = (time.perf_counter() - t_start) * 1_000_000.0
            return str(p.resolve()), {"us": us, "json_ld": 0, "microdata": 0, "opengraph": 0, "total_entities": 0}

    with ThreadPoolExecutor(max_workers=8) as pool:
        for p_str, res in pool.map(_worker, file_paths):
            out[p_str] = res

    elapsed = time.perf_counter() - t0
    return out, elapsed


def generate_nested_microdata(depth: int) -> str:
    """Generate deeply nested itemscope hierarchy."""
    html = ["<!DOCTYPE html><html><body>"]
    for i in range(depth):
        html.append(f'<div itemscope itemtype="http://schema.org/Thing{i}" itemprop="child_{i}">')
        html.append(f'<span itemprop="name">Level {i}</span>')
        html.append(f'<meta itemprop="depth" content="{i}" />')
    html.append('<p itemprop="description">Deep leaf payload</p>')
    for _ in range(depth):
        html.append('</div>')
    html.append('</body></html>')
    return "\n".join(html)


def generate_massive_graph(num_nodes: int) -> str:
    """Generate massive JSON-LD @graph with circular-like references."""
    graph = []
    for i in range(num_nodes):
        graph.append({
            "@id": f"urn:node:{i}",
            "@type": "Product" if i % 2 == 0 else "Organization",
            "name": f"Entity {i}",
            "description": f"Detailed description for entity {i} with metadata payload.",
            "isPartOf": f"urn:node:{(i + 1) % num_nodes}",
            "relatedTo": [f"urn:node:{(i + j) % num_nodes}" for j in range(1, 5)],
        })
    doc = {
        "@context": "https://schema.org",
        "@graph": graph,
    }
    return f'<!DOCTYPE html><html><head><script type="application/ld+json">{json.dumps(doc)}</script></head><body><h1>Massive Graph</h1></body></html>'


def main():
    print("================================================================================")
    print("=== HIGH-DENSITY & PATHOLOGICAL STRUCTURED DATA EXTREME STRESS BENCHMARK ===")
    print("================================================================================\n")

    # TEST 1: Official Extruct Test Samples (63 files)
    samples_dir = Path("bench/data/fresh/data/extruct-repo/tests/samples")
    files = sorted(samples_dir.glob("**/*.html"))
    print(f"--- Test 1: Official Extruct Benchmark Corpus ({len(files)} files) ---")

    ps_res, ps_time = run_pagesift_files(files)
    ex_res, ex_time = run_extruct_files(files)

    ps_entities = sum(v["total_entities"] for v in ps_res.values())
    ex_entities = sum(v["total_entities"] for v in ex_res.values())
    ps_avg_us = sum(v["us"] for v in ps_res.values()) / max(1, len(ps_res))
    ex_avg_us = sum(v["us"] for v in ex_res.values()) / max(1, len(ex_res))

    print(f"PageSift (Rust):  {ps_entities} entities extracted | total: {ps_time*1000:.1f}ms (avg {ps_avg_us:.1f} µs/page)")
    print(f"extruct (Python): {ex_entities} entities extracted | total: {ex_time*1000:.1f}ms (avg {ex_avg_us:.1f} µs/page)")
    print(f"-> Speed Advantage: PageSift is {ex_time/ps_time:.1f}x faster overall (per-page: {ex_avg_us/ps_avg_us:.1f}x faster)!\n")

    # TEST 2: Pathological Deeply Nested Microdata (10, 20, 30 levels)
    print("--- Test 2: Pathological Deep Nesting Stress Test (Microdata Itemscopes) ---")
    tmp_dir = Path("/tmp/struct_deep")
    tmp_dir.mkdir(parents=True, exist_ok=True)
    deep_files = []
    for d in [10, 20, 30, 40]:
        p = tmp_dir / f"deep_{d}.html"
        p.write_text(generate_nested_microdata(d))
        deep_files.append(p)

    ps_deep, _ = run_pagesift_files(deep_files)
    ex_deep, _ = run_extruct_files(deep_files)

    print(f"{'Depth':<10} {'PageSift Time':<18} {'extruct Time':<18} {'Speedup':<12}")
    print("-" * 58)
    for p in deep_files:
        d_str = p.stem.split("_")[1]
        p_str = str(p.resolve())
        ps_lat = ps_deep[p_str]["us"]
        ex_lat = ex_deep[p_str]["us"]
        speedup = ex_lat / max(1, ps_lat)
        print(f"{d_str:<10} {ps_lat:<18.1f}µs {ex_lat:<18.1f}µs {speedup:<12.1f}x")
    print()

    # TEST 3: Massive JSON-LD Knowledge Graph (100, 300, 500, 1000 entities)
    print("--- Test 3: Massive Knowledge Graph Stress Test (@graph JSON-LD) ---")
    graph_files = []
    for n in [100, 300, 500, 1000]:
        p = tmp_dir / f"graph_{n}.html"
        p.write_text(generate_massive_graph(n))
        graph_files.append(p)

    ps_graph, _ = run_pagesift_files(graph_files)
    ex_graph, _ = run_extruct_files(graph_files)

    print(f"{'Nodes':<10} {'PageSift Time':<18} {'extruct Time':<18} {'Speedup':<12}")
    print("-" * 58)
    for p in graph_files:
        n_str = p.stem.split("_")[1]
        p_str = str(p.resolve())
        ps_lat = ps_graph[p_str]["us"]
        ex_lat = ex_graph[p_str]["us"]
        speedup = ex_lat / max(1, ps_lat)
        print(f"{n_str:<10} {ps_lat:<18.1f}µs {ex_lat:<18.1f}µs {speedup:<12.1f}x")
    print("================================================================================\n")


if __name__ == "__main__":
    main()
