# Six-project comparison

The current [report](REPORT.md), [40-category inventory](CAPABILITIES.md) and [public evidence index](evidence/README.md) describe bounded comparisons with pinned Firecrawl, Crawlee, Scrapling, Katana, Essence and gallery-dl versions. Components, complete self-hosted services and synthetic tasks are scored separately. Full functional leadership is not established.

This directory contains only the current report, compact protocols/per-page scores, reproducible drivers, runtime locks and minimal adapters. Historical round reports, exploratory scripts, raw website bodies, downloaded sources, logs, binaries and build outputs remain local under ignored `bench/data/` or `bench/six/results/` paths.

## Resource-bounded runs

`run.py` caps aggregate tasks at **8 logical CPUs / 8 GiB**, disables task swap and reserves system memory and disk. Every outcome is retained locally; incomplete or duplicate sample IDs fail validation. Use a fresh checkout/output directory for a new reproduction: existing `seven-next` results are intentionally not overwritten.

```bash
python3 bench/six/run.py --id sources --timeout 600 -- python3 bench/six/prepare_tools.py
python3 bench/six/run.py --id runtimes --timeout 1200 -- python3 bench/six/bootstrap.py
python3 bench/six/run.py --id components --timeout 1200 -- python3 bench/six/build_tools.py
```

`tools.lock.json` fixes tags and source SHAs. `locks/` records the measured Python/npm environments, native binary hashes and Firecrawl image digests. Source checkouts and installed dependencies go only into `bench/data/six/`. The benchmark has its own Python/Node runtime; product cores remain Rust.

## Current drivers

| Driver | Scope |
|---|---|
| `seven_corpus.py` | All 545 calibrated WebMainBench pages, common annotation stripping and diagnostic token-multiset F1 |
| `seven_service.py` + `seven_service_resilient.py` | Shared replay inputs, failed-page retention and service restart before advancing |
| `firecrawl_stack.py` + `firecrawl_full.py` | Disposable, resource-bounded five-container self-hosted reference |
| `seven_tasks.py` | Native CLI navigation, forms, relocation, media originals and finite VOD decoding controls |
| `sdk.py` | Official Python/JS Firecrawl SDK calls against an isolated local server |
| `full_capability_inventory.py` | Source-based capability categories and explicit verification gaps |

For the content corpus, download the exact file URL in [the frozen protocol](evidence/2026-10-22/content/protocol.json) to `bench/data/six/seven-next/WebMainBench_545.jsonl`, and verify its SHA-256 before preparing. The original raw and gold text are not distributed here. A fresh reproduction cannot claim no overlap with earlier inputs unless all four earlier input manifests are available; the driver marks that check as unknown when absent.

```bash
# Build current Rust binaries inside the bounded runner; respect local resource limits.
python3 bench/six/run.py --id candidate --timeout 1200 -- \
  cargo build --release --locked -p pagesift
python3 bench/six/run.py --id content-example --timeout 1200 -- \
  cargo build --release --locked -p pagesift-content --example extract_jsonl
python3 bench/six/run.py --id corpus --timeout 300 -- \
  bench/data/six/venv/bin/python bench/six/seven_corpus.py prepare
python3 bench/six/run.py --id content-pagesift --timeout 1800 -- \
  bench/data/six/venv/bin/python bench/six/seven_corpus.py run pagesift-component target/release/examples/extract_jsonl
python3 bench/six/run.py --id content-essence --timeout 1800 -- \
  bench/data/six/venv/bin/python bench/six/seven_corpus.py run essence-component bench/data/six/bin/offline-stable
```

The service/task drivers expect an explicitly copied candidate at `bench/six/results/seven-next/bin-verified/pagesift`. Services use dedicated ports, volumes and data directories. FFmpeg/ffprobe are fixture-generation and decoding tools for the VOD controls; the product does not invoke them. Inspect each protocol before running external references.

Frozen evidence describes its measured candidate and environment. Version bumps and release checks do not relabel those scores as a new benchmark. The public artifact manifest verifies hashes without shipping local experiment logs or build outputs.
