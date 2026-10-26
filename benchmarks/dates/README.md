# Metadata benchmarks

## Publication dates (`dates.py`)

Dataset: the human-annotated evaluation set shipped with [htmldate](https://github.com/adbar/htmldate)
(Apache-2.0): `eval_default.json` (800 pages, multilingual with a focus on German, BBAW collection)
and `eval_mediacloud_2020.json` (200 worldwide news pages, Data Culture Group, Northeastern University).
Clone it into `benchmarks/data/fresh/data/htmldate` (`git clone --depth 1 https://github.com/adbar/htmldate`).

Scoring follows htmldate's own `tests/comparison.py`: the date is compared at day granularity
(`YYYY-MM-DD`); an empty answer counts as a true negative when the page has no date and as a false
negative otherwise; a wrong date is a false positive. Precision, recall, accuracy and F1 are higher
is better; time per page is lower is better. By default no URL is given to any tool (htmldate's
setting; tools may use `<link rel=canonical>`); `--url` also passes the page URL.

Pages are split by `sha1(url) % 2` into a tuning half and a hold-out half; a change to pagesift is
kept only if the hold-out half does not get worse.

```bash
cargo build --release -p pagesift --example meta_eval
benchmarks/data/fresh/tools/py/bin/python -I benchmarks/dates/dates.py            # all tools
benchmarks/data/fresh/tools/py/bin/python -I benchmarks/dates/dates.py --tools pagesift --dump /tmp/d.json
cargo run --release -p pagesift --example date_trace -- page.html     # what each step finds
cargo run --release -p pagesift --example date_scan -- dir...         # crash scan + time per step
```

pagesift's time in `dates.py` is the whole `process()` call (Markdown, metadata, links); `date_scan`
reports the date search alone.

## Install

Competitors (htmldate, trafilatura, newspaper4k, goose3) are in the shared `py` environment
([Tool environments](../README.md#tool-environments)); the data is the htmldate clone above. The 0.70
structured-data harness (`scripts/d7_*.py`, `adapters/metaext`) uses extruct from the same environment;
build the adapter with `cargo build --release --manifest-path benchmarks/dates/adapters/metaext/Cargo.toml`.

## Dry run (2026-10-27)

`dates.py --tools pagesift` over the full 1,000 pages ran and printed the precision/recall table (the
competitors were not rerun).
