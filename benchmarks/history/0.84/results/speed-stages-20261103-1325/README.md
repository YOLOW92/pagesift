# Stage timings, 0.81.5 vs 0.90.0 interleaved, 16 concurrent requests

`benchmarks/history/0.84/stages.py <serve> <site> <pages> 16` against the same local site as `speed.sh`, after one
warm-up pass; the two binaries alternate, four rounds each. Columns: pages per second (higher is better), median
client latency and the median of each server-side stage from the response `timings` (ms, lower is better).

| Round | Binary | pages/s | client ms | extract | fetch | queue | total |
|---|---|---|---|---|---|---|---|
| 1 | 0.81.5 | 328.2 | 26.544 | 1.85 | 22.12 | 0.0 | 24.9 |
| 1 | 0.90.0 | 943.5 | 6.045 | 1.6 | 2.77 | 0.0 | 5.01 |
| 2 | 0.81.5 | 845.8 | 7.435 | 1.93 | 3.38 | 0.0 | 5.99 |
| 2 | 0.90.0 | 927.6 | 7.561 | 1.73 | 3.69 | 0.0 | 6.09 |
| 3 | 0.81.5 | 792.4 | 8.092 | 1.95 | 3.95 | 0.0 | 6.69 |
| 3 | 0.90.0 | 1325.9 | 4.9 | 1.55 | 2.08 | 0.0 | 3.93 |
| 4 | 0.81.5 | 304.8 | 30.216 | 1.82 | 26.24 | 0.0 | 28.94 |
| 4 | 0.90.0 | 821.5 | 7.137 | 1.62 | 3.41 | 0.0 | 5.97 |

In the rounds near 300 pages/s only the `fetch` stage (time to get the page from the local Python site server)
grows; `extract` and `queue` do not change. The same happens to either binary (`speed-20261103-132110`: 0.81.5 round 4,
0.90.0 rounds 1, 4, 5).
