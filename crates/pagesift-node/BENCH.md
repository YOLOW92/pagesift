# Node 包基准：pagesift 和 Readability + Turndown 对比

同一批 HTML：`bench/data/inputs`（`pagesift bench --emit-inputs` 冻结的评测集，597 页，179 MB）。
Readability 需要 DOM，用 JSDOM 解析，解析时间算在耗时里（和 `bench/compare/js_adapter.mjs` 的用法一致）。
每个工具在单独的子进程里运行；先预热 20 页。

运行（在仓库根目录）：

```bash
node crates/pagesift-node/bench/bench.mjs bench/data/inputs/manifest.jsonl bench/data/node
```

## 结果（2026-10-21，linux-x64，16 线程，Node v24.21.0，release 构建）

| 指标 | pagesift (Node) | Readability + Turndown | 对比 |
|---|---|---|---|
| 单页耗时 p50，ms（越低越好） | 2.31 | 187.71 | 快 81 倍 |
| 单页耗时 p95，ms（越低越好） | 10.51 | 1352.86 | 快 129 倍 |
| 单页耗时均值，ms（越低越好） | 4.05 | 407.92 | 快 101 倍 |
| 吞吐，页/秒（越高越好） | 1263 | 2.5 | 515 倍 |
| 进程峰值内存 RSS，MB（越低越好） | 734 | 4303 | 低 83% |

说明：

- 单页耗时是主线程上逐页同步调用（pagesift 用 `convert`）。
- 吞吐：pagesift 把整批页面一次交给 `convertAsync`，在 16 个 libuv 线程上并行（`UV_THREADPOOL_SIZE=16`）；
  Readability + Turndown 是纯 JS，只能在一个线程上跑，吞吐就是单页均值的倒数。
- pagesift 的内存峰值来自吞吐测试：597 页的 HTML 和结果同时在内存里。
- 测量时机器上还有其他会话在编译（load average 5～9），绝对数字偏慢，倍数仅供参考；发布前要在空闲机器上重跑。
- 这里只测速度；内容质量用 `pagesift bench --external` 打分（`--out` 写出对应的 JSON lines）。
