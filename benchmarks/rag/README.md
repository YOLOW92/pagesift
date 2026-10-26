# Chunking and query filtering for RAG

Data: Chroma's [chunking_evaluation](https://github.com/brandonstarxel/chunking_evaluation) (MIT): five
corpora (financial filings, PubMed papers, Wikipedia text, a State of the Union address, chat logs;
1.4 MB) and 472 questions, each labelled with the exact character spans of its answer. Download the
`questions_df.csv` and `corpora/*.md` files into `benchmarks/data/fresh/data/chunkeval`.

Questions are split by `sha1(question) % 2` into a tuning half and a hold-out half; design choices
were made on the tuning half and kept only if the hold-out half agreed.

## Chunking (`eval.py`)

Every chunker cuts the five corpora into chunks of about 200 words (word count is the shared size
measure). One retriever is used for all of them: `bm25s` with English stop words over all chunks of
all corpora. For each question, chunks are taken in score order until a word budget (250, 500,
1,000 words) is reached; a chunk larger than the remaining budget counts only up to the budget.
Recall (share of answer characters retrieved), precision (share of retrieved characters that are
answer) and IoU are averaged over questions; all are higher is better.

Chunkers: pagesift (`recursive`, `fixed_tokens`, `sliding_window`, `paragraphs`, `sentences`),
LangChain `RecursiveCharacterTextSplitter`, chonkie (`RecursiveChunker`, `SentenceChunker`,
`TokenChunker`; the Markdown recipe needs a HuggingFace download and is not run), semchunk, and
Crawl4AI (`FixedLengthWordChunking`, `SlidingWindowChunking`, `RegexChunking`).

## Query filter (`qf_*.py`)

Each question is asked against its own document (on average 49,000 words). "Default" rows are
what each tool returns with its default settings: recall, words returned (lower is better) and
precision. Ranked rows compare scoring units at equal word budgets (100, 250, 500 words).

```bash
benchmarks/rag/run.sh      # results in benchmarks/rag/out/{chunking,query_filter,timing}.txt
```

## Install

Chunkers and the retriever are in the shared `py` environment (bm25s, langchain-text-splitters,
chonkie, semchunk) and Crawl4AI in `c4ai` ([Tool environments](../README.md#tool-environments));
`run.sh` builds pagesift's `chunk_spans` and `qf_ranks` examples itself. QASPER runs use the same
script with `DATA=benchmarks/data/fresh/data/qasper-eval`. The 0.70 D5 harness (`scripts/d5_py.py`,
`adapters/d5eval`) builds with `cargo build --release --manifest-path benchmarks/rag/adapters/d5eval/Cargo.toml`.

## Dry run (2026-10-27)

`OUT=<scratch dir> benchmarks/rag/run.sh` ran end to end (2 minutes) and wrote `chunking.txt`,
`query_filter.txt` and `timing.txt`. `run.sh` also asks for a LlamaIndex chunker
(`llamaindex-sentence`), but `llama-index` is not installed on the benchmark machine and is not in
`versions.md`, so that row reports "unavailable". The defaults for `OUT`, `DATA`, `PY` and `C4A` still
pointed at `bench/…` and are fixed in this round.
