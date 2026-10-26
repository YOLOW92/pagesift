# Shared scripts

| Path | What it is |
| :--- | :--- |
| [fresh/](fresh/) | Release-run harness across domains (`run_domains.sh`, `run_extract.sh`, `pinned.sh`) and the dated results of the 0.30.0 and 2026-10-22 runs |
| [fetch_snapshots.py](fetch_snapshots.py) | Polite snapshot fetcher used by `pagesift bench` |
| [showdown_run.py](showdown_run.py) | Runs one tool in a container under a fixed memory cap (0.70 showdown) |

## Dry run (2026-10-27)

- `fetch_snapshots.py <dataset.csv> <out dir>` fetches every URL over the network; not run.
- `showdown_run.py --help` works. It runs tools in the `pagesift-bench:*` Docker images, whose
  Dockerfiles were in `bench/docker` and have been removed, so it cannot be rerun as is.
- `fresh/run_domains.sh` and `fresh/run_extract.sh` need the whole machine (and `run_extract.sh prep`
  rewrites `benchmarks/data/inputs`); not run. Their per-domain steps were dry-run in each domain's
  README.
