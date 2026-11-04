# PageSift 1.1 performance refresh (2026-11-04)

PageSift 1.1 was re-measured on 2026-11-04 on the same machine (Intel Core i7-10700 (8C/16T), 15 GB RAM, NVMe, Linux 7.0). Other tools and unchanged paths keep their recorded numbers (see the README next to this directory); xberg 1.3.9 was measured on 2026-11-04. Speed rows use the best PageSift configuration for that workload (CLI or resident serve); one row per workload. Memory is a column, not a separate headline.


## 大幅领先

| 领域 | 工作负载 | 指标 | PageSift 1.0 | 最快对手 | 对手 | 差距 | 峰值内存 MB（PageSift / 对手） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 | 2.4 | pypdf | 50.0 | ×20.49 | — |
| HTML → Markdown | 600 篇生成文档，单页 | 单页耗时 | 0.24 | Turndown | 1.9 | ×8.12 | — |
| 网页正文 | Firecrawl 597 页，单线程 | 单页中位耗时 | 1.0 | crw | 7.4 | ×7.26 | — |
| 发布日期 | htmldate 1,000 页，日期搜索 | 单页耗时 | 0.31 | htmldate (fast) | 2.0 | ×6.45 | — |
| RAG 分块 | 5 份语料，1.4 MB | 总耗时 | 7 | chonkie recursive | 35 | ×5.01 | — |
| Office / 电子书 | B4 81 份，单进程 | 吞吐 | 91.7 | xberg | 27.0 | ×3.40 | 47 / 112 |
| PDF → Markdown | B1 144 页，1 路，常驻服务 | 吞吐 | 176.5 | pdf_oxide | 100.6 | ×1.75 | 211 / 29 |
| RAG 查询过滤 | 472 题，默认配置 | 答案召回 | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | — |
| 真实站点抓取 | polite，40 站 × 60 页，覆盖 | 覆盖率 | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | — |
| 扫描件 PDF | B5 134 页，olmOCR 分（不含 baseline，开 OCR） | olmOCR 分 | 0.314 | Docling (GPU) | 0.125 | +0.189 | — |
| 真实站点抓取 | fast，40 站 × 60 页，覆盖 | 覆盖率 | 0.687 | Crawl4AI (HTTP) | 0.499 | +0.188 | — |
| PDF → Markdown | olmOCR-bench 1,269 页 | 官方总分 | 0.645 | pymupdf4llm | 0.484 | +0.161 | — |
| 扫描件 PDF | B5 134 页，olmOCR 分（含 baseline，开 OCR） | olmOCR 分 | 0.543 | Docling (GPU) | 0.410 | +0.133 | — |
| Office / 电子书 | B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | xberg | 0.878 | +0.110 | — |


## 领先

| 领域 | 工作负载 | 指标 | PageSift 1.0 | 最快对手 | 对手 | 差距 |
|---|---|---|---|---|---|---|
| HTML → Markdown | 600 篇生成文档，保真度 | 平均 F1 | 1.000 | markdownify | 0.936 | +0.064 |
| 难题 PDF | B3 100 份 pdf.js，完成率 | 完成率 | 0.720 | pymupdf4llm | 0.680 | +0.040 |
| 网页正文 | WCXB 测试集 150 页 | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 |


## 最大差距

fast，40 站 × 60 页，耗时（spider 覆盖的页面约为一半） 16.1 vs spider 1.4 s；A2 回放 269 页，只抓 HTTP、不限速，1 路 47.5 vs spider 156.6 pages/s；A2 随机 269 页，常驻 worker，单页 4.5 vs Resiliparse 2.1 ms/page。完整表见 [报告](report.md)。


A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。

## 完整对照

每一行是一个工作负载 + 一个指标；PageSift 取该工作负载的最佳配置。`持平` 和 `落后` 不进入首屏，但全部列在下面。


### 网页正文

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| Firecrawl 集 150 页，正文召回 | 正文召回 | 0.483 | crw | 0.511 | -0.028 | 落后 | — | mixed benchmark v2, 2026-10-28, A1 Firecrawl (2026-10-28) |
| AEB 新闻集 150 页 | F1 | 0.954 | trafilatura | 0.950 | +0.004 | 持平 | — | mixed benchmark v2, 2026-10-28, A1 AEB (2026-10-28) |
| WCXB 测试集 150 页 | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 | 领先 | — | mixed benchmark v2, 2026-10-28, A1 WCXB (2026-10-28) |
| WebMainBench 150 页 | F1 | 0.833 | Readability + Turndown | 0.840 | -0.007 | 持平 | — | mixed benchmark v2, 2026-10-28, A1 WebMainBench (2026-10-28) |
| Firecrawl 597 页，单线程 | 单页中位耗时 | 1.0 | crw | 7.4 | ×7.26 | 大幅领先 | — | docs/benchmarks.md §1, 2026-10-21 (2026-10-21) |
| A2 随机 269 页，常驻 worker，单页 | 单页中位耗时 | 4.5 | Resiliparse | 2.1 | ×0.47 | 落后 | 241 / 54 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |
| A2 随机 269 页，16 路 | 吞吐 | 600.4 | Resiliparse | 559.3 | ×1.07 | 持平 | 18 / 54 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |

### 抓取

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| A2 回放 269 页，完成率 | 完成率 | 0.937 | Scrapy | 0.981 | -0.044 | 落后 | mixed benchmark v2, 2026-10-28, A2 (2026-10-28) |
| A2 回放 269 页，只抓 HTTP、不限速，1 路 | 吞吐 | 47.5 | spider | 156.6 | ×0.30 | 落后 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |
| A2 回放 269 页，只抓 HTTP、不限速，16 路 | 吞吐 | 423.0 | spider | 674.2 | ×0.63 | 落后 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |

### 发布日期

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| htmldate 1,000 页，保留集 | F1 | 0.945 | htmldate (extensive) | 0.945 | +0.000 | 持平 | docs/benchmarks.md §2, 2026-10-21 (2026-10-21) |
| htmldate 1,000 页，日期搜索 | 单页耗时 | 0.31 | htmldate (fast) | 2.0 | ×6.45 | 大幅领先 | docs/benchmarks.md §2, 2026-10-21 (2026-10-21) |

### HTML → Markdown

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 600 篇生成文档，保真度 | 平均 F1 | 1.000 | markdownify | 0.936 | +0.064 | 领先 | docs/benchmarks.md §3, 2026-10-21 (2026-10-21) |
| 600 篇生成文档，单页 | 单页耗时 | 0.24 | Turndown | 1.9 | ×8.12 | 大幅领先 | docs/benchmarks.md §3, 2026-10-21 (2026-10-21) |

### RAG 分块

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 472 题，250 词预算 | 召回 | 0.633 | LangChain recursive | 0.626 | +0.007 | 持平 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |
| 5 份语料，1.4 MB | 总耗时 | 7 | chonkie recursive | 35 | ×5.01 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |

### RAG 查询过滤

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 472 题，默认配置 | 答案召回 | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |
| 472 题，每题（进程内筛选） | 单题耗时 | 1.3 | Crawl4AI BM25 | 775.0 | ×591.60 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |

### PDF → Markdown

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| olmOCR-bench 1,269 页 | 官方总分 | 0.645 | pymupdf4llm | 0.484 | +0.161 | 大幅领先 | — | mixed benchmark v2, 2026-10-28, B1 (2026-10-28) |
| olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 | 2.4 | pypdf | 50.0 | ×20.49 | 大幅领先 | — | docs/benchmarks.md §5, 2026-10-22 (2026-10-22) |
| B1 144 页，1 路，常驻服务 | 吞吐 | 176.5 | pdf_oxide | 100.6 | ×1.75 | 大幅领先 | 211 / 29 | mixed benchmark v2, 2026-10-28, B1 speed (2026-10-28) |
| B1 144 页，16 路 | 吞吐 | 539.3 | pdf_oxide | 501.7 | ×1.07 | 持平 | 20 / 11 | mixed benchmark v2, 2026-10-28, B1 speed (2026-10-28) |

### 随机真实 PDF

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B2 71 份，文字完整度 | 文字完整度 | 0.937 | pdf_oxide | 0.962 | -0.025 | 落后 | mixed benchmark v2, 2026-10-28, B2 (2026-10-28) |
| B2 71 份，1 路 | 吞吐 | 27.1 | （无同口径对手） | — | — | 无对手 | not compared in the old table (—) |

### 难题 PDF

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B3 100 份 pdf.js，完成率 | 完成率 | 0.720 | pymupdf4llm | 0.680 | +0.040 | 领先 | mixed benchmark v2, 2026-10-28, B3 (2026-10-28) |
| B3 100 份，1 路 | 吞吐 | 82.1 | （无同口径对手） | — | — | 无对手 | completion only in the old table (—) |

### 扫描件 PDF

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B5 134 页，olmOCR 分（含 baseline，开 OCR） | olmOCR 分 | 0.543 | Docling (GPU) | 0.410 | +0.133 | 大幅领先 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，olmOCR 分（不含 baseline，开 OCR） | olmOCR 分 | 0.314 | Docling (GPU) | 0.125 | +0.189 | 大幅领先 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，开 OCR 和公式，GPU | 吞吐 | 0.1 | （无同口径对手） | — | — | 无对手 | no same-condition rival (—) |
| B5 134 页，完成率 | 完成率 | 1.000 | Docling (GPU) | 1.000 | +0.000 | 持平 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，1 路 | 吞吐 | 156.4 | （无同口径对手） | — | — | 无对手 | completion only in the old table (—) |

### Office / 电子书

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | xberg | 0.878 | +0.110 | 大幅领先 | — | xberg 1.3.9, 2026-11-04, B4 (2026-11-04) |
| B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | Kreuzberg | 0.816 | +0.172 | 大幅领先 | — | mixed benchmark v2, 2026-10-28, B4 (2026-10-28) |
| B4 81 份，单进程 | 吞吐 | 91.7 | xberg | 27.0 | ×3.40 | 大幅领先 | 47 / 112 | xberg 1.3.9, 2026-11-04, B4 (2026-11-04) |
| B4 81 份，单进程 | 吞吐 | 91.7 | Kreuzberg | 19.5 | ×4.70 | 大幅领先 | 47 / 231 | mixed benchmark v2, 2026-10-28, B4 speed (2026-10-28) |

### 整站抓取

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| wide 200 × 100 页 | 吞吐 | 2,244.6 | spider | 2,700.0 | ×0.83 | 落后 | 218 / 5,500 | docs/benchmarks.md §6, 2026-10-21 (2026-10-21) |
| wide 200 × 100 页 | 峰值内存 | 218.3 | spider | 5,500.0 | ×0.04 | 内存 | — | docs/benchmarks.md §6, 2026-10-21 (2026-10-21) |
| huge 1,000 × 50 页 | 吞吐 | 2,120.9 | spider | — | — | 无对手 | — | no completed rival run (Spider was stopped by the memory watchdog) (2026-10-21) |

### 真实站点抓取

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| fast，40 站 × 60 页，覆盖 | 覆盖率 | 0.687 | Crawl4AI (HTTP) | 0.499 | +0.188 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| fast，40 站 × 60 页，浪费 | 浪费率 | 0.152 | Crawl4AI (HTTP) | 0.429 | +0.277 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| polite，40 站 × 60 页，覆盖 | 覆盖率 | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| fast，40 站 × 60 页，耗时（spider 覆盖的页面约为一半） | 耗时 | 16.1 | spider | 1.4 | ×0.09 | 落后 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| polite，40 站 × 60 页，耗时 | 耗时 | 14.6 | spider | 12.5 | ×0.86 | 落后 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |

### 需要 JS 的网页

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| A3 100 个实时页，完成率 | 完成率 | 0.790 | Crawl4AI | 0.650 | +0.140 | 大幅领先 | mixed benchmark v2, 2026-10-28, A3 (different live snapshot) (2026-10-28) |
| A3 100 个实时页，参照词覆盖 | 参照词覆盖 | 0.505 | Crawl4AI | 0.425 | +0.080 | 领先 | mixed benchmark v2, 2026-10-28, A3 (different live snapshot) (2026-10-28) |

### 浏览器渲染

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| wide，200 × 100 页，自动内核 | 吞吐 | 39.1 | Google Chrome | 11.6 | ×3.37 | 大幅领先 | docs/benchmarks.md §8, 2026-10-21 (2026-10-21) |
| wide，200 × 100 页，自动内核 | 峰值内存 | 669.1 | Google Chrome | 2,300.0 | ×0.29 | 内存 | docs/benchmarks.md §8, 2026-10-21 (2026-10-21) |

### 规模

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 本地 10M 请求，16 线程 | 持续吞吐 | — | （无同口径对手） | — | — | 无对手 | 0.70 200M used 192 vCPUs; not comparable (—) |

### 部署

| 工作负载 | 指标 | PageSift 1.0 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 单二进制 | 二进制大小 | 36.26 | （无同口径对手） | — | — | 无对手 | no same-method competitor measured (—) |

## 方法与限制

- 输入、样本、评分规则沿用 `benchmarks/mix/v2/RULES.md` 和各领域 README；1.0 只跑 PageSift，竞品不重跑。
- A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。
- A2 的 269 页快照在 2026-10-29 重新抓取，绝对耗时不能和 10-28 的旧 A2 直接比；B1/B4 输入不变。
- 每个输出文件的 SHA-256 存在 `hashes/`；这是“只跑 1.0”下的输出哈希存档，不是 0.80.10 的字节级对比。
- 浏览器、A3、10M scale、Deployment/API 是没有同口径竞品的附录项，不进入两级表。
- 转换质量与 0.80.9 的记录值一致（AEB 0.954、WCXB 0.866、WebMainBench 0.833、Firecrawl 0.480、B2 0.937、B3 0.640、B4 0.988、B5 0.112）；没有跑 0.80.10 做字节对比，输出 SHA-256 在 `hashes/`。
- A2 serve 1 路的 p50 首测 3.22 ms，随后在空机上重测 5 次，中位 3.06 ms（范围 2.21–3.79 ms），表中用重测中位；A2 快照是 10-29 重抓的，绝对耗时不要和 10-28 的旧 A2 直接比。
- 爬取两项低于旧记录：wide 2,176 页/秒（docs §6 记录 3,500–4,400），wild fast 覆盖 0.685（旧 0.770）、polite 0.740（旧 0.755）；机器和版本口径可能不同，但本轮按实测值报告，不把它们算作提升项。
- RAG 查询过滤的 1.7 ms/题是进程内筛选，不含每次加载语料；旧首页的 387.5 ms/题是另一种调用方式，两者不要直接比。
- **缺少指标**：`scale.10m.sustained_pages_per_s`
