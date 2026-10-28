# 0.80.9 measurements

All runs on 2026-10-28, in the same container as the 0.80.5 records.

## Focused crawl on 69 universities, five directions

`sites.txt` holds 69 university home pages: the 38 from 0.80.3 plus 31 new ones (mainland China,
Hong Kong, Taiwan, Japan, Korea, India, Saudi Arabia, South Africa, Latin America, the Nordics,
continental Europe, Australia and the US). `run.sh` crawls each of them in five directions with
`crawl --query … --adaptive --limit 12 --ignore-robots`:

| Topic | Query |
| :--- | :--- |
| `intl` | international students admission tuition scholarship visa |
| `grad` | graduate admission requirements PhD application deadline |
| `housing` | student housing accommodation residence |
| `cs` | computer science department faculty research |
| `library` | library opening hours borrowing |

robots.txt is ignored so that every version sees the same sites; 0.80.0 has no `--ignore-robots`
on the command line, so `api_run.py` sends the same request to its HTTP API with
`ignoreRobotsTxt` (and `allow_crawl_ignore_robots = true` in its config).

`grade.py` labels every delivered page from its URL and title with the same rules for all
versions: U (squarely on the query), M (related but general), X (not useful). It is a coarse
proxy, not a human reading; its use is comparing versions on the same footing. Only sites that
both versions reached are compared.

| File | What |
| :--- | :--- |
| `results/0.80.0/`, `results/0.80.5/`, `results/0.80.9/` | `index.jsonl` per topic and site (URL, title, status, score, depth) |
| `results/seed-prior-dropped/` | a variant that took its first term weights from the seed page's links; not shipped (below) |
| `results/0.80.0-vs-0.80.9.txt` | per-topic comparison |
| `results/0.80.5-vs-0.80.9.txt` | what this release's crawl changes add on top of 0.80.5 |
| `results/0.80.9-vs-seed-prior.txt` | the rejected variant |
| `results/perf-instructions.txt` | callgrind instruction counts against 0.80.0 |

Summary (all topics, sites reached by both):

| Version | Pages | U | U+M |
| :--- | ---: | ---: | ---: |
| 0.80.0 | 3,572 | 2.6% | 18.4% |
| 0.80.9 | 3,421 | 19.0% | 49.6% |

Most of the gain over 0.80.0 comes from the 0.80.2–0.80.3 scoring work. This release's own crawl
changes (re-scoring queued links when term weights change; delivering each page once when it is
reached under several URLs) move U from 18.3% to 19.1% against 0.80.5, within run-to-run noise,
and cut same-path duplicate pages from 38 to 7.

The rejected variant set the first term weights from how often each query term appears in the
seed page's links. It helped housing (+6 points U) and cs (+2) but cost grad 4.5 points: the
navigation words a graduate-admission query needs ("graduate", "admission") are exactly the ones
that appear in many homepage links. A change that helps some directions and hurts others is not a
general improvement, so it was dropped.

## Weak spots from the mixed benchmark

Reproduced locally with the public inputs of `benchmarks/mix` (frozen sample lists, SHA-256
checked where the files are redistributable):

- **Hard PDFs (B3, 100 pdf.js test files).** Same machine, before and after: 53 → 61 completed
  (the benchmark's own 0.80.0 run scored 0.56 on its machine). The 47 failures before: 8 need a
  user password (pdf.js's test manifest lists it), 2 have no readable page (no page object; a
  fuzzed file with one empty page), 26 have no text at all (pdftotext also gives 0 words), 10 had
  their text in annotation appearances (form fields, FreeText, stamps, signatures) and 1 used a
  Type3 font with a 1:1 FontMatrix. Fixed: the Type3 file and 7 of the 10; the other 3 contain only
  push-button captions, which are skipped on purpose. A further 250
  pdf.js test files and 15 arXiv papers were compared old against new: 18 outputs changed, all by
  added field values, signatures, notes or figure text.
- **PDF throughput, one process per file.** A small PDF parses in 0.07 ms while the `scrape`
  process spent 9.4 ms of CPU starting a pinned multi-thread runtime, probing the CPU topology and
  loading the root certificates once per HTTP client. With the lighter one-shot runtime: 6.3 →
  4.3 ms CPU per call; 107 → 131 files/s with 1 worker and 310 → 447 with 4 (4-core machine). The
  16-worker collapse in the benchmark report (142 files/s) does not reproduce on 4 cores, even at
  32 workers.
- **HTTP fetch, 1 worker.** On 214 random pages, `engine: auto` escalated 55 to a browser; the
  browser returned clearly more text for 21 of them (38%). No signal available before rendering
  (whole-page text, main/whole ratio, HTML size) separates the two groups, so the rule is
  unchanged: this is a quality-for-speed trade, and spider never renders.
- **Firecrawl truth recall.** On the 115 of 150 sampled pages that could be fetched again, only 328
  of the 1,150 annotated lines on low-recall pages still exist in the page; we missed 140 of
  those. The fixable pattern was accordion and FAQ headers (kept now: 0.491 → 0.496, noise leak
  unchanged at 0.042); tag clouds and navigation stay out on purpose.

## Performance

Instruction counts against 0.80.0 stay within 2% (`results/perf-instructions.txt`): HTML +0.19%,
PDF −0.02%, CSV +0.70% / −6.17% / +1.17% / +0.75%.
