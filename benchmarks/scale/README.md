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
