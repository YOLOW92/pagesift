# `/v2/agent` test bench

A closed test: local pages, local search, a question set with known answers. Nothing is fetched from the live web;
with a real model, the only outside traffic is the calls to the model service.

| File | What |
|---|---|
| `questions.jsonl` | 22 questions: expected values per field (`fields`), the page holding the answer (`gold`), recorded replies (`recorded`) |
| `corpus_server.rs` | Example `agent_corpus`: serves `benchmarks/data/snapshots` with scripts, styles, images and outbound links removed, and indexes it with the existing BM25 behind a SearXNG-shaped `/search`. `/robots.txt` disallows `/private/` |
| `run.py` | Starts the corpus, `pagesift serve` and (for recorded runs) a fake model service, runs every question through `POST /v2/agent`, scores it, writes `results/` |
| `results/recorded.json` | Result of the recorded run |

The snapshots are not committed (245 MB, 833 files; 766 have text after cleaning). Every request sets
`allowedHosts: ["127.0.0.1"]`, so a model that names another URL is refused.

Besides plain lookups the set covers: two pages for one question (q16), no answer in the corpus (q18), the only
source disallowed by robots.txt (q19), `urls` without search (q20), a Chinese question (q21), the budget ending
after one search (q22), and the `extract` tool (q23).

## Run

```bash
cargo build --profile quick -p pagesift --bin pagesift --example agent_corpus

# Recorded replies
python3 -I benchmarks/history/1.0/agent/run.py

# A real model: the key is read from the environment and never written to a file
GROQ_API_KEY=... python3 -I benchmarks/history/1.0/agent/run.py \
    --base-url https://api.groq.com/openai/v1 --model openai/gpt-oss-120b --effort low
```

Use `--snapshots DIR` (or `PAGESIFT_AGENT_SNAPSHOTS`) when the snapshots are elsewhere, `--target-dir` and
`--profile` for the binaries, `--only q01,q02` and `--limit N` for a subset. Results are saved after each question,
and `--resume` skips the questions already answered, which helps when a daily token quota runs out.

Scoring: a question is correct when every field contains an accepted value (case and punctuation ignored). A
question with no answer needs the field empty and `complete: false`; q19 also needs `agent_robots_disallowed`, and
q22 `stopReason: credit_limit_reached`.

## Recorded run

2026-11-04, quick build. **This checks the flow, not quality.** The recorded replies contain the expected answers,
so the 22 of 22 correct says nothing about how well a model does; no real model has been run on this set. Tokens are
pagesift's estimate (bytes / 3) because the fake service reports no usage, and exclude a real model's reasoning.

| Measure | Result |
|---|---|
| Questions finishing with the expected status and fields | 22 of 22 |
| Pages per question (↓) | mean 0.95, max 2 |
| Estimated tokens per question (↓) | mean 3164 |
| Model calls per question (↓) | mean 3.0 |
| Expected page in the first 5 search results (↑) | all 17 questions that search for one |
| Fields whose value was found in the evidence (↑) | 27 of 28 |
| Questions with the expected page among the field sources (↑) | 18 of 19 |

The two values that do depend on pagesift rather than the replies are the last two rows, since provenance is checked
against the text the run archived. The miss is q10: the phone number did not reach the evidence for the recorded query.
q11 reads the same page with a different query and its value, next to the phone number on that page, did.
