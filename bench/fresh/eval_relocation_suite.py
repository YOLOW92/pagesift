#!/usr/bin/env python3
"""DOM Relocation & Self-Healing Baseline Benchmark Suite.

Compares:
1. PageSift (pure Rust FuzzyAnchor)
2. Scrapling (Python Adaptor.relocate)

Tests across 6 mutation categories:
- Scenario 1: Class Hashing (Tailwind / CSS Modules / Styled-Components)
- Scenario 2: Tag Mutation (span -> p, div -> article, a -> button)
- Scenario 3: Wrapper Insertion (Extra container divs / sections)
- Scenario 4: Attribute Stripping & Renaming
- Scenario 5: Sibling Reordering with Decoys
- Scenario 6: Combined Heavy Redesign (Tag + Hash + Wrapper + Shift)
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

try:
    from scrapling.parser import Adaptor
    HAS_SCRAPLING = True
except ImportError:
    HAS_SCRAPLING = False

CASES = [
    # 1. Class Hashing
    {
        "id": "hash_01_product_price",
        "category": "Class Hashing",
        "selector": ".price",
        "expected": "$2,499.00",
        "orig": '<div class="product"><h1 class="title">Laptop</h1><span class="price">$2,499.00</span></div>',
        "mut": '<div class="product-wrap"><h1 class="c-title-81">Laptop</h1><span class="c-val-x98f">$2,499.00</span></div>',
    },
    {
        "id": "hash_02_author_name",
        "category": "Class Hashing",
        "selector": ".author-name",
        "expected": "Dr. Linus Pauling",
        "orig": '<header><div class="byline"><span class="author-name">Dr. Linus Pauling</span></div></header>',
        "mut": '<header><div class="css-1a2b3c"><span class="css-9z8y7x">Dr. Linus Pauling</span></div></header>',
    },
    {
        "id": "hash_03_cta_button",
        "category": "Class Hashing",
        "selector": ".btn-checkout",
        "expected": "Complete Purchase",
        "orig": '<div class="actions"><button class="btn-checkout primary">Complete Purchase</button></div>',
        "mut": '<div class="css-box-12"><button class="css-btn-submit-45">Complete Purchase</button></div>',
    },
    # 2. Tag Mutation
    {
        "id": "tag_01_span_to_p",
        "category": "Tag Mutation",
        "selector": ".stock-status",
        "expected": "In Stock (Ships in 24 hours)",
        "orig": '<div class="inventory"><span class="stock-status">In Stock (Ships in 24 hours)</span></div>',
        "mut": '<div class="inventory"><p class="stock-status">In Stock (Ships in 24 hours)</p></div>',
    },
    {
        "id": "tag_02_div_to_article",
        "category": "Tag Mutation",
        "selector": ".main-post",
        "expected": "Deep Reinforcement Learning in Web Exploration",
        "orig": '<div class="main-post"><h2>Deep Reinforcement Learning in Web Exploration</h2></div>',
        "mut": '<article class="main-post"><h2>Deep Reinforcement Learning in Web Exploration</h2></article>',
    },
    {
        "id": "tag_03_button_to_a",
        "category": "Tag Mutation",
        "selector": ".action-download",
        "expected": "Download Dataset (CSV)",
        "orig": '<div class="toolbar"><button class="action-download">Download Dataset (CSV)</button></div>',
        "mut": '<div class="toolbar"><a href="/dl" class="action-download">Download Dataset (CSV)</a></div>',
    },
    # 3. Wrapper Insertion
    {
        "id": "wrap_01_nested_cards",
        "category": "Wrapper Insertion",
        "selector": "div.card > span.rating",
        "expected": "4.8 out of 5 stars (1,240 reviews)",
        "orig": '<div class="card"><h3 class="name">Widget</h3><span class="rating">4.8 out of 5 stars (1,240 reviews)</span></div>',
        "mut": '<div class="card"><div class="card-inner-wrap"><div class="meta-section"><span class="rating">4.8 out of 5 stars (1,240 reviews)</span></div></div></div>',
    },
    {
        "id": "wrap_02_table_responsive",
        "category": "Wrapper Insertion",
        "selector": "table.data-grid td.total-revenue",
        "expected": "$14,892,100",
        "orig": '<table class="data-grid"><tr><td class="total-revenue">$14,892,100</td></tr></table>',
        "mut": '<div class="table-responsive-container"><div class="scroll-wrapper"><table class="data-grid"><tr><td class="total-revenue">$14,892,100</td></tr></table></div></div>',
    },
    # 4. Attribute Stripping & Renaming
    {
        "id": "attr_01_strip_id",
        "category": "Attribute Stripping",
        "selector": "#product-sku-code",
        "expected": "SKU-98234-XJ",
        "orig": '<div class="specs"><span id="product-sku-code" class="code">SKU-98234-XJ</span></div>',
        "mut": '<div class="specs"><span class="code" data-ref="ref-12">SKU-98234-XJ</span></div>',
    },
    {
        "id": "attr_02_data_attribute_migration",
        "category": "Attribute Stripping",
        "selector": "[data-qa='user-email']",
        "expected": "lead_researcher@mit.edu",
        "orig": '<div class="profile"><span data-qa="user-email" class="email">lead_researcher@mit.edu</span></div>',
        "mut": '<div class="profile"><span data-testid="user-email-field" class="email">lead_researcher@mit.edu</span></div>',
    },
    # 5. Sibling Reordering & Decoys
    {
        "id": "sibling_01_decoy_prices",
        "category": "Sibling Reordering",
        "selector": ".featured-offer .price",
        "expected": "$199 special deal",
        "orig": '<div class="featured-offer"><span class="label">Primary</span><span class="price">$199 special deal</span></div><div class="sidebar"><span class="price">$499 standard</span></div>',
        "mut": '<div class="sidebar"><span class="price">$499 standard</span></div><div class="banner"><span class="badge">Sale</span></div><div class="featured-offer"><span class="price">$199 special deal</span><span class="label">Primary</span></div>',
    },
    {
        "id": "sibling_02_footer_decoy",
        "category": "Sibling Reordering",
        "selector": "#main-content p.abstract",
        "expected": "This paper presents a formal analysis of web scrapers.",
        "orig": '<main id="main-content"><p class="abstract">This paper presents a formal analysis of web scrapers.</p></main><footer><p class="abstract">Disclaimer: All rights reserved.</p></footer>',
        "mut": '<footer><p class="abstract">Disclaimer: All rights reserved.</p></footer><section id="main-content"><article><p class="abstract">This paper presents a formal analysis of web scrapers.</p></article></section>',
    },
    # 6. Combined Heavy Redesign
    {
        "id": "combined_01_ecom_full_redesign",
        "category": "Combined Heavy Redesign",
        "selector": "div.product-details span.discount-price",
        "expected": "$89.99 (Save 40%)",
        "orig": '<div class="product-details"><h2 class="title">Wireless Earbuds</h2><span class="discount-price">$89.99 (Save 40%)</span></div>',
        "mut": '<section class="c-pdp-layout"><div class="c-col-main"><div class="c-info-card"><h1 class="c-hdr-bold">Wireless Earbuds</h1><p class="c-val-accent">$89.99 (Save 40%)</p></div></div></section>',
    },
    {
        "id": "combined_02_job_posting",
        "category": "Combined Heavy Redesign",
        "selector": "div.job-header span.salary-range",
        "expected": "$180,000 - $220,000 / yr",
        "orig": '<div class="job-header"><h3>Staff Systems Engineer</h3><span class="salary-range">$180,000 - $220,000 / yr</span></div>',
        "mut": '<main class="app-view"><div class="layout-stack"><header class="posting-meta"><h2>Staff Systems Engineer</h2><div class="badge-row"><p class="text-emerald-600 font-semibold">$180,000 - $220,000 / yr</p></div></header></div></main>',
    },
]


def run_scrapling(cases: list[dict]) -> tuple[list[dict], float]:
    if not HAS_SCRAPLING:
        return [], 0.0
    t0 = time.perf_counter()
    results = []
    for c in cases:
        t_start = time.perf_counter()
        orig_page = Adaptor(c["orig"])
        try:
            target_el = orig_page.css(c["selector"])[0]
            mut_page = Adaptor(c["mut"])
            relocated = mut_page.relocate(target_el)
            lat_us = (time.perf_counter() - t_start) * 1_000_000.0
            found_text = relocated[0].text.strip() if relocated else ""
            ok = c["expected"] in found_text or found_text in c["expected"]
            results.append({
                "id": c["id"],
                "category": c["category"],
                "found": bool(relocated),
                "ok": ok,
                "text": found_text,
                "us": lat_us,
            })
        except Exception:
            lat_us = (time.perf_counter() - t_start) * 1_000_000.0
            results.append({
                "id": c["id"],
                "category": c["category"],
                "found": False,
                "ok": False,
                "text": "",
                "us": lat_us,
            })
    elapsed = time.perf_counter() - t0
    return results, elapsed


def run_pagesift(cases: list[dict], ps_bin: str) -> tuple[list[dict], float]:
    tmp_jsonl = Path("/tmp/ps_relocate_in.jsonl")
    with tmp_jsonl.open("w") as out:
        for c in cases:
            row = {
                "id": c["id"],
                "selector": c["selector"],
                "original_html": c["orig"],
                "mutated_html": c["mut"],
            }
            out.write(json.dumps(row) + "\n")

    t0 = time.perf_counter()
    proc = subprocess.run([ps_bin, str(tmp_jsonl)], capture_output=True, text=True, check=True)
    elapsed = time.perf_counter() - t0

    out_map = {}
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 5:
            cid, found, text, score, us = parts[0], parts[1] == "true", parts[2], float(parts[3]), float(parts[4])
            out_map[cid] = (found, text, score, us)

    results = []
    for c in cases:
        cid = c["id"]
        found, text, score, us = out_map.get(cid, (False, "", 0.0, 0.0))
        text = text.strip()
        ok = found and (c["expected"] in text or text in c["expected"])
        results.append({
            "id": cid,
            "category": c["category"],
            "found": found,
            "ok": ok,
            "text": text,
            "us": us,
        })
    return results, elapsed


def main():
    print("================================================================================")
    print("=== DOM RELOCATION & SELF-HEALING BASELINE BENCHMARK ===")
    print("================================================================================\n")

    ps_bin = "target/release/examples/bench_relocation"
    if not Path(ps_bin).exists():
        sys.exit(f"Binary {ps_bin} not found. Run cargo build --release -p pagesift-extract --example bench_relocation")

    ps_res, ps_time = run_pagesift(CASES, ps_bin)
    sc_res, sc_time = run_scrapling(CASES)

    total = len(CASES)
    ps_corr = sum(1 for r in ps_res if r["ok"])
    sc_corr = sum(1 for r in sc_res if r["ok"])
    ps_avg_us = sum(r["us"] for r in ps_res) / total
    sc_avg_us = sum(r["us"] for r in sc_res) / total

    print(f"{'Engine':<20} {'Success Rate':<18} {'Avg Latency':<16} {'Speedup':<12}")
    print("-" * 66)
    print(f"{'PageSift (Current)':<20} {ps_corr}/{total} ({ps_corr/total*100:.1f}%)      {ps_avg_us:<14.1f}µs {'1.0x (Baseline)':<12}")
    print(f"{'Scrapling (Adaptor)':<20} {sc_corr}/{total} ({sc_corr/total*100:.1f}%)      {sc_avg_us:<14.1f}µs {sc_avg_us/ps_avg_us:.1f}x slower")
    print("=" * 66 + "\n")

    # Category Breakdown
    cats = sorted(set(c["category"] for c in CASES))
    print(f"{'Mutation Category':<28} {'PageSift':<16} {'Scrapling':<16}")
    print("-" * 62)
    for cat in cats:
        ps_c = sum(1 for r in ps_res if r["category"] == cat and r["ok"])
        sc_c = sum(1 for r in sc_res if r["category"] == cat and r["ok"])
        cat_t = sum(1 for r in CASES if r["category"] == cat)
        print(f"{cat:<28} {ps_c}/{cat_t} ({ps_c/cat_t*100:.1f}%)      {sc_c}/{cat_t} ({sc_c/cat_t*100:.1f}%)")
    print("================================================================================\n")


if __name__ == "__main__":
    main()
