# Scale and multi-core scaling

| Path | What it is |
| :--- | :--- |
| [p0/](p0/) | Synthetic local origin and runners (NUMA executors, HTTP reactor, sharded and adaptive runners) |
| [p3/](p3/) | `run_100m_gate.sh` and `run_200m_check.sh` for the 100M and 200M saturation runs |
| [autotune/](autotune/) | 1 → 192 vCPU scaling sweep and USL fit |
| [scripts/](scripts/) | Local 100M run, Common Crawl WARC throughput and the 0.70 E1 stress suite |
| [results/](results/) | Logs and JSON from the 0.70 runs |

Reports: [200M saturation](../../docs/history/0.70_200m_regression.md),
[CPU scaling (USL)](../../docs/history/0.70_p1_cpu_scaling_usl.md),
[100M saturation](../../docs/history/0.70_p3_100m_saturation.md). All are synthetic local-origin runs
without network, not real-internet speeds.

## Build and rerun

```bash
cargo build --release --offline --manifest-path benchmarks/scale/p0/Cargo.toml   # stream_origin and the runners
cargo build --release --offline --manifest-path benchmarks/scale/p3/Cargo.toml
benchmarks/scale/p3/run_200m_check.sh <tag>     # 200M run: whole machine, systemd --user, writes results/0.70_200m_<tag>.*
benchmarks/scale/scripts/local_100m.sh          # 100M on a 16-core machine; needs jq
```

`p0/Cargo.lock` still named the pagesift crates with old versions; building updates it to
the current workspace versions (third-party versions unchanged). `p3` has no lock file in the repository.

## Dry run (2026-10-27)

Both crates built. `stream_origin --total-pages 100000` with `p2_http_reactor_runner --formal --mode full
--split-pipeline --total-tasks 100000` completed 100,000/100,000 tasks, 0 failed, and wrote its JSON
report. Not run: the 100M/200M scripts (whole machine), `autotune/sweep_cores.py` (a 1 → 192 vCPU
sweep) and the E1 / Common Crawl scripts. `run_100m_gate.sh` and `run_200m_check.sh` computed the
repository root one level too high after the move; fixed in this round. Like the other 0.70 scripts
under `scripts/`, `local_100m.sh` starts with `cd ~/pagesift`, so it runs against the checkout at
`~/pagesift`, not the one it lives in.
