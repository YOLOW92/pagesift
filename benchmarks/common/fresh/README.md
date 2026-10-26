# PageSift 0.50.0 Fresh Benchmark & Evaluation Suite

This directory contains standalone, reproducible Python evaluation scripts used to generate the verified results for PageSift 0.50.0.

## Benchmark Scripts

| Script | Domain | Target Competitors | Description |
| :--- | :--- | :--- | :--- |
| [`eval_1m_real_web.py`](eval_1m_real_web.py) | **Real-World Web (100k - 1M)** | `Spider 2.53.9` | Tranco Top 100k global live crawl evaluation, memory telemetry, conversion rate, and deduplication. |
| [`bench_d4_dom_healing.py`](bench_d4_dom_healing.py) | **D4. DOM Self-Healing** | `Scrapling` | Evaluates element relocation recall, false positive rejection, and deep nested DOM resistance. |
| [`bench_d5_tier2_stress.py`](bench_d5_tier2_stress.py) | **D5. RAG Chunking & BM25** | `semchunk`, `Crawl4AI` | Evaluates SIMD semantic chunking latency and BM25 inverted index query filtering. |
| [`eval_d6_050.py`](eval_d6_050.py) | **D6. Charset Detection** | `chardet`, `charset-normalizer` | Evaluates multi-language legacy and UTF-8 encoding detection accuracy and throughput. |
| [`eval_d7_050.py`](eval_d7_050.py) | **D7. Structured Data** | `extruct` | Tests microdata/JSON-LD parsing speed and memory safety under 40-level deep DOM trees. |
| [`eval_d8_050.py`](eval_d8_050.py) | **D8. Implicit JS Endpoints** | `Katana 1.8.0`, `jsluice` | Evaluates endpoint discovery recall and rejection of non-navigational decoy links. |
| [`eval_d9_050.py`](eval_d9_050.py) | **D9. Dynamic Headless (CDP)**| `Playwright`, `Lightpanda` | Measures warm-pool headless Chrome rendering latency and canvas/SVG rendering fidelity. |
| [`eval_d10_050.py`](eval_d10_050.py) | **D10. AI API Gateways** | `Portkey`, `LiteLLM` | Measures gateway request throughput (req/s), P99 latency, and backpressure behavior under 429 floods. |

All results and summary reports are automatically exported to [`benchmarks/history/0.50/results/`](../../history/0.50/results/).
