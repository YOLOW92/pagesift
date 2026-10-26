# Shared scripts

| Path | What it is |
| :--- | :--- |
| [fresh/](fresh/) | Release-run harness across domains (`run_domains.sh`, `run_extract.sh`, `pinned.sh`) and the dated results of the 0.30.0 and 2026-10-22 runs |
| [fetch_snapshots.py](fetch_snapshots.py) | Polite snapshot fetcher used by `pagesift bench` |

## Dry run (2026-10-27)

- `fetch_snapshots.py <dataset.csv> <out dir>` fetches every URL over the network; not run.
- `showdown_run.py` and the other Docker-based 0.70 runners moved to
  [history/0.70/docker/](../history/0.70/docker/README.md): their images have been removed.
- `fresh/run_domains.sh` and `fresh/run_extract.sh` need the whole machine (and `run_extract.sh prep`
  rewrites `benchmarks/data/inputs`); not run. Their per-domain steps were dry-run in each domain's
  README.
