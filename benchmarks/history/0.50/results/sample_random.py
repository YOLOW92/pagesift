#!/usr/bin/env python3
"""Random Real-World Sample Generator for Phase 3.1.
Samples domains from Tranco-B with a recorded random seed, pairing with real-world HTML snapshots.
"""
import csv, json, random, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TRANCO_CSV = ROOT / "bench/data/tranco-b.csv"
SNAPSHOTS_DIR = ROOT / "bench/data/snapshots-b"
CRAWL_DIR = ROOT / "bench/data/crawl-pages-b"
OUT_DIR = ROOT / "bench/results/final"

SEED = 893421774
random.seed(SEED)

def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    
    # 1. Read Tranco Domains
    domains = []
    if TRANCO_CSV.exists():
        with open(TRANCO_CSV, "r", encoding="utf-8") as f:
            reader = csv.reader(f)
            for row in reader:
                if len(row) >= 2:
                    rank, domain = row[0].strip(), row[1].strip()
                    domains.append({"rank": int(rank) if rank.isdigit() else 0, "domain": domain})
    
    # 2. Collect Real HTML Files
    html_files = sorted(list(SNAPSHOTS_DIR.glob("*.html")) + list(CRAWL_DIR.glob("*.html")))
    print(f"Found {len(domains)} Tranco domains and {len(html_files)} real-world HTML snapshots.")
    
    # 3. Uniform Random Sample of 300 Distinct Targets
    sampled_domains = random.sample(domains, min(300, len(domains)))
    sampled_htmls = random.sample(html_files, min(300, len(html_files)))
    
    manifest_rows = []
    for i, (dom, html_path) in enumerate(zip(sampled_domains, sampled_htmls)):
        row = {
            "id": f"sample_{i+1:04d}",
            "domain": dom["domain"],
            "rank": dom["rank"],
            "path": str(html_path.resolve()),
            "url": f"https://{dom['domain']}/article/{i+1}",
            "base_url": f"https://{dom['domain']}",
            "bytes": html_path.stat().st_size
        }
        manifest_rows.append(row)
        
    manifest_file = OUT_DIR / "sample_manifest.jsonl"
    with open(manifest_file, "w", encoding="utf-8") as f:
        for r in manifest_rows:
            f.write(json.dumps(r) + "\n")
            
    seed_file = OUT_DIR / "seed.json"
    seed_info = {
        "master_seed": SEED,
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime()),
        "sample_count": len(manifest_rows),
        "source": "bench/data/tranco-b.csv",
        "snapshots_pool_size": len(html_files)
    }
    with open(seed_file, "w", encoding="utf-8") as f:
        json.dump(seed_info, f, indent=2)
        
    print(f"Sample manifest generated: {manifest_file} ({len(manifest_rows)} items)")
    print(f"Seed file generated: {seed_file}")

if __name__ == "__main__":
    main()
