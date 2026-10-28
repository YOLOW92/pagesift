# 0.80.3 real-site verification (read-only)

The measurements behind [docs/releases/0.80.3.md](../../../docs/releases/0.80.3.md): the focused
crawl on 38 real university home pages, 0.80.0 against 0.80.3, from a Linux x86_64 cloud container
on 2026-10-28 (robots.txt honoured; the container's proxy refuses plain `http://` requests).

| Path | What it is |
| :--- | :--- |
| `sites.txt` | The 38 start pages (name, URL) |
| `run.sh` | `run.sh <pagesift binary> <out dir>`: `crawl --query "international students admission tuition scholarship visa" --adaptive --limit 12 --out` for every site, four at a time |
| `grade.py` | `grade.py <out dir>…`: labels each crawled page U (international students), M (admissions/study) or X by URL and title, and prints per-site counts and the totals |
| `results/<version>/<site>.jsonl` | The crawl's `index.jsonl` per site (0.80.0 has only url and title; 0.80.2 and 0.80.3 add depth, parent, score, relevance) |
| `results/<version>/<site>.log` | The crawl's stderr without the per-page file lines: errors, stop reason |
| `results/<version>-graded.txt` | `grade.py`'s per-site counts |
| `results/perf-instructions.txt` | Instruction counts (callgrind) of the HTML, PDF and CSV benchmarks, 0.80.0 against 0.80.3 |
| `results/tum-attachments-index.jsonl` | `crawl https://www.international.tum.de/ --query … --adaptive --limit 12 --attachments` on 0.80.3 |

`0.80.0` is the v0.80.0 tag built in release mode; `0.80.2` is the v0.80.2 release build;
`0.80.3-run1` and `-run2` are two runs of the 0.80.3 release build. The release notes compare the
32 sites every version reached.
