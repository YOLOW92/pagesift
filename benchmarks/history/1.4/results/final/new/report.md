# PageSift 1.4 performance refresh (2026-11-14)

PageSift 1.4 was measured on 2026-11-14 on the same machine (Intel Core i7-10700 (8C/16T), 15 GB RAM, NVMe, Linux 7.0). Local offline workloads (B1/B3/B4/B5 quality & speed, olmOCR-bench B1, Office B4 with `yolowdoc 0.1.1`, B5 OCR GPU, HTML -> Markdown 600, API, format smoke, `yolowsyntax 0.1.2`, `yolowzstd 0.1.2`, and `DataGraph`) were newly measured on PageSift 1.4.0; deleted-corpus/live-web rows keep the recorded PageSift 1.0/1.1 values (see Methods & Limits). Competitor numbers keep their recorded 2026-10-21/22/27/28 (plus xberg 1.3.9 2026-11-04) baselines. Speed rows use the best PageSift configuration for that workload (CLI or resident serve); one row per workload. Memory is a column, not a separate headline.


## 大幅领先

| 领域 | 工作负载 | 指标 | PageSift 1.4 | 最快对手 | 对手 | 差距 | 峰值内存 MB（PageSift / 对手） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 | 2.5 | pypdf | 50.0 | ×20.08 | — |
| HTML → Markdown | 600 篇生成文档，单页 | 单页耗时 | 0.23 | Turndown | 1.9 | ×8.36 | — |
| 网页正文 | Firecrawl 597 页，单线程 | 单页中位耗时 | 1.0 | crw | 7.4 | ×7.26 | — |
| 发布日期 | htmldate 1,000 页，日期搜索 | 单页耗时 | 0.31 | htmldate (fast) | 2.0 | ×6.45 | — |
| RAG 分块 | 5 份语料，1.4 MB | 总耗时 | 7 | chonkie recursive | 35 | ×5.01 | — |
| Office / 电子书 | B4 81 份，单进程 | 吞吐 | 81.9 | xberg | 27.0 | ×3.03 | 42 / 112 |
| PDF → Markdown | B1 144 页，1 路，常驻服务 | 吞吐 | 224.0 | pdf_oxide | 100.6 | ×2.23 | 211 / 29 |
| RAG 查询过滤 | 472 题，默认配置 | 答案召回 | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | — |
| 真实站点抓取 | polite，40 站 × 60 页，覆盖 | 覆盖率 | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | — |
| 扫描件 PDF | B5 134 页，olmOCR 分（不含 baseline，开 OCR） | olmOCR 分 | 0.314 | Docling (GPU) | 0.125 | +0.189 | — |
| 真实站点抓取 | fast，40 站 × 60 页，覆盖 | 覆盖率 | 0.687 | Crawl4AI (HTTP) | 0.499 | +0.188 | — |
| PDF → Markdown | olmOCR-bench 1,269 页 | 官方总分 | 0.645 | pymupdf4llm | 0.484 | +0.161 | — |
| 扫描件 PDF | B5 134 页，olmOCR 分（含 baseline，开 OCR） | olmOCR 分 | 0.543 | Docling (GPU) | 0.410 | +0.133 | — |
| Office / 电子书 | B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | xberg | 0.878 | +0.110 | — |


## 领先

| 领域 | 工作负载 | 指标 | PageSift 1.4 | 最快对手 | 对手 | 差距 | 峰值内存 MB（PageSift / 对手） |
|---|---|---|---|---|---|---|---|
| 网页正文 | A2 随机 269 页，16 路 | 吞吐 | 656.1 | Resiliparse | 559.3 | ×1.17 | 17 / 54 |
| HTML → Markdown | 600 篇生成文档，保真度 | 平均 F1 | 1.000 | markdownify | 0.936 | +0.064 | — |
| 难题 PDF | B3 100 份 pdf.js，完成率 | 完成率 | 0.720 | pymupdf4llm | 0.680 | +0.040 | — |
| 网页正文 | WCXB 测试集 150 页 | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 | — |


## 最大差距

fast，40 站 × 60 页，耗时（spider 覆盖的页面约为一半） 16.1 vs spider 1.4 s；A2 回放 269 页，只抓 HTTP、不限速，1 路 47.5 vs spider 156.6 pages/s；A2 随机 269 页，常驻 worker，单页 5.2 vs Resiliparse 2.1 ms/page。完整表见 [报告](report.md)。


A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。

## 完整对照

每一行是一个工作负载 + 一个指标；PageSift 取该工作负载的最佳配置。`持平` 和 `落后` 不进入首屏，但全部列在下面。


### 网页正文

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| Firecrawl 集 150 页，正文召回 | 正文召回 | 0.483 | crw | 0.511 | -0.028 | 落后 | — | mixed benchmark v2, 2026-10-28, A1 Firecrawl (2026-10-28) |
| AEB 新闻集 150 页 | F1 | 0.954 | trafilatura | 0.950 | +0.004 | 持平 | — | mixed benchmark v2, 2026-10-28, A1 AEB (2026-10-28) |
| WCXB 测试集 150 页 | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 | 领先 | — | mixed benchmark v2, 2026-10-28, A1 WCXB (2026-10-28) |
| WebMainBench 150 页 | F1 | 0.833 | Readability + Turndown | 0.840 | -0.007 | 持平 | — | mixed benchmark v2, 2026-10-28, A1 WebMainBench (2026-10-28) |
| Firecrawl 597 页，单线程 | 单页中位耗时 | 1.0 | crw | 7.4 | ×7.26 | 大幅领先 | — | docs/benchmarks.md §1, 2026-10-21 (2026-10-21) |
| A2 随机 269 页，常驻 worker，单页 | 单页中位耗时 | 5.2 | Resiliparse | 2.1 | ×0.41 | 落后 | 244 / 54 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |
| A2 随机 269 页，16 路 | 吞吐 | 656.1 | Resiliparse | 559.3 | ×1.17 | 领先 | 17 / 54 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |

### 抓取

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| A2 回放 269 页，完成率 | 完成率 | 0.937 | Scrapy | 0.981 | -0.044 | 落后 | mixed benchmark v2, 2026-10-28, A2 (2026-10-28) |
| A2 回放 269 页，只抓 HTTP、不限速，1 路 | 吞吐 | 47.5 | spider | 156.6 | ×0.30 | 落后 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |
| A2 回放 269 页，只抓 HTTP、不限速，16 路 | 吞吐 | 423.0 | spider | 674.2 | ×0.63 | 落后 | mixed benchmark v2, 2026-10-28, A2 speed (2026-10-28) |

### 发布日期

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| htmldate 1,000 页，保留集 | F1 | 0.945 | htmldate (extensive) | 0.945 | +0.000 | 持平 | docs/benchmarks.md §2, 2026-10-21 (2026-10-21) |
| htmldate 1,000 页，日期搜索 | 单页耗时 | 0.31 | htmldate (fast) | 2.0 | ×6.45 | 大幅领先 | docs/benchmarks.md §2, 2026-10-21 (2026-10-21) |

### HTML → Markdown

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 600 篇生成文档，保真度 | 平均 F1 | 1.000 | markdownify | 0.936 | +0.064 | 领先 | docs/benchmarks.md §3, 2026-10-21 (2026-10-21) |
| 600 篇生成文档，单页 | 单页耗时 | 0.23 | Turndown | 1.9 | ×8.36 | 大幅领先 | docs/benchmarks.md §3, 2026-10-21 (2026-10-21) |

### RAG 分块

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 472 题，250 词预算 | 召回 | 0.633 | LangChain recursive | 0.626 | +0.007 | 持平 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |
| 5 份语料，1.4 MB | 总耗时 | 7 | chonkie recursive | 35 | ×5.01 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |

### RAG 查询过滤

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 472 题，默认配置 | 答案召回 | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |
| 472 题，每题（进程内筛选） | 单题耗时 | 1.3 | Crawl4AI BM25 | 775.0 | ×591.60 | 大幅领先 | docs/benchmarks.md §4, 2026-10-21 (2026-10-21) |

### PDF → Markdown

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| olmOCR-bench 1,269 页 | 官方总分 | 0.645 | pymupdf4llm | 0.484 | +0.161 | 大幅领先 | — | mixed benchmark v2, 2026-10-28, B1 (2026-10-28) |
| olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 | 2.5 | pypdf | 50.0 | ×20.08 | 大幅领先 | — | docs/benchmarks.md §5, 2026-10-22 (2026-10-22) |
| B1 144 页，1 路，常驻服务 | 吞吐 | 224.0 | pdf_oxide | 100.6 | ×2.23 | 大幅领先 | 211 / 29 | mixed benchmark v2, 2026-10-28, B1 speed (2026-10-28) |
| B1 144 页，16 路 | 吞吐 | 475.2 | pdf_oxide | 501.7 | ×0.95 | 落后 | 18 / 11 | mixed benchmark v2, 2026-10-28, B1 speed (2026-10-28) |

### 随机真实 PDF

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B2 71 份，文字完整度 | 文字完整度 | 0.937 | pdf_oxide | 0.962 | -0.025 | 落后 | mixed benchmark v2, 2026-10-28, B2 (2026-10-28) |
| B2 71 份，1 路 | 吞吐 | 27.1 | （无同口径对手） | — | — | 无对手 | not compared in the old table (—) |

### 难题 PDF

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B3 100 份 pdf.js，完成率 | 完成率 | 0.720 | pymupdf4llm | 0.680 | +0.040 | 领先 | mixed benchmark v2, 2026-10-28, B3 (2026-10-28) |
| B3 100 份，1 路 | 吞吐 | 74.6 | （无同口径对手） | — | — | 无对手 | completion only in the old table (—) |

### 扫描件 PDF

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| B5 134 页，olmOCR 分（含 baseline，开 OCR） | olmOCR 分 | 0.543 | Docling (GPU) | 0.410 | +0.133 | 大幅领先 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，olmOCR 分（不含 baseline，开 OCR） | olmOCR 分 | 0.314 | Docling (GPU) | 0.125 | +0.189 | 大幅领先 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，开 OCR 和公式，GPU | 吞吐 | 0.1 | （无同口径对手） | — | — | 无对手 | no same-condition rival (—) |
| B5 134 页，完成率 | 完成率 | 1.000 | Docling (GPU) | 1.000 | +0.000 | 持平 | mixed benchmark v2, 2026-10-28, B5 (2026-10-28) |
| B5 134 页，1 路 | 吞吐 | 134.8 | （无同口径对手） | — | — | 无对手 | completion only in the old table (—) |

### Office / 电子书

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | xberg | 0.878 | +0.110 | 大幅领先 | — | xberg 1.3.9, 2026-11-04, B4 (2026-11-04) |
| B4 留出集 39 份，结构保留率 | 结构保留率 | 0.988 | Kreuzberg | 0.816 | +0.172 | 大幅领先 | — | mixed benchmark v2, 2026-10-28, B4 (2026-10-28) |
| B4 81 份，单进程 | 吞吐 | 81.9 | xberg | 27.0 | ×3.03 | 大幅领先 | 42 / 112 | xberg 1.3.9, 2026-11-04, B4 (2026-11-04) |
| B4 81 份，单进程 | 吞吐 | 81.9 | Kreuzberg | 19.5 | ×4.20 | 大幅领先 | 42 / 231 | mixed benchmark v2, 2026-10-28, B4 speed (2026-10-28) |

### 整站抓取

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 峰值内存 MB（PageSift / 对手） | 对手数据 |
|---|---|---:|---|---:|---:|---|---|---|
| wide 200 × 100 页 | 吞吐 | 2,244.6 | spider | 2,700.0 | ×0.83 | 落后 | 218 / 5,500 | docs/benchmarks.md §6, 2026-10-21 (2026-10-21) |
| wide 200 × 100 页 | 峰值内存 | 218.3 | spider | 5,500.0 | ×0.04 | 内存 | — | docs/benchmarks.md §6, 2026-10-21 (2026-10-21) |
| huge 1,000 × 50 页 | 吞吐 | 2,120.9 | spider | — | — | 无对手 | — | no completed rival run (Spider was stopped by the memory watchdog) (2026-10-21) |

### 真实站点抓取

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| fast，40 站 × 60 页，覆盖 | 覆盖率 | 0.687 | Crawl4AI (HTTP) | 0.499 | +0.188 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| fast，40 站 × 60 页，浪费 | 浪费率 | 0.152 | Crawl4AI (HTTP) | 0.429 | +0.277 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| polite，40 站 × 60 页，覆盖 | 覆盖率 | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | 大幅领先 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| fast，40 站 × 60 页，耗时（spider 覆盖的页面约为一半） | 耗时 | 16.1 | spider | 1.4 | ×0.09 | 落后 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |
| polite，40 站 × 60 页，耗时 | 耗时 | 14.6 | spider | 12.5 | ×0.86 | 落后 | docs/benchmarks.md §7, 2026-10-21 (2026-10-21) |

### 需要 JS 的网页

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| A3 100 个实时页，完成率 | 完成率 | 0.790 | Crawl4AI | 0.650 | +0.140 | 大幅领先 | mixed benchmark v2, 2026-10-28, A3 (different live snapshot) (2026-10-28) |
| A3 100 个实时页，参照词覆盖 | 参照词覆盖 | 0.505 | Crawl4AI | 0.425 | +0.080 | 领先 | mixed benchmark v2, 2026-10-28, A3 (different live snapshot) (2026-10-28) |

### 浏览器渲染

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| wide，200 × 100 页，自动内核 | 吞吐 | 39.1 | Google Chrome | 11.6 | ×3.37 | 大幅领先 | docs/benchmarks.md §8, 2026-10-21 (2026-10-21) |
| wide，200 × 100 页，自动内核 | 峰值内存 | 669.1 | Google Chrome | 2,300.0 | ×0.29 | 内存 | docs/benchmarks.md §8, 2026-10-21 (2026-10-21) |

### 规模

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 本地 10M 请求，16 线程 | 持续吞吐 | — | （无同口径对手） | — | — | 无对手 | 0.70 200M used 192 vCPUs; not comparable (—) |

### 部署

| 工作负载 | 指标 | PageSift 1.4 | 对手 | 对手值 | 对比 | 级别 | 对手数据 |
|---|---|---:|---|---:|---:|---|---|
| 单二进制 | 二进制大小 | 49.41 | （无同口径对手） | — | — | 无对手 | no same-method competitor measured (—) |

## 1.4 新增与独立依赖实测

### 独立 crate 路径（`yolowsyntax 0.1.2` / `yolowzstd 0.1.2` / `yolowdoc 0.1.1` / `formats`）

| 项目 | 指标 | 实测值 |
|---|---|---|
| `pagesift-syntax` (`yolowsyntax 0.1.2`, 8 语言源码) | 中位吞吐 / 区间 / 无损 | 16.115 MB/s (10.420–25.760 MB/s), lossless=1, peak RSS 21.21 MB |
| `pagesift-compress` (`yolowzstd 0.1.2`, Silesia 4 文件) | 解压 / L3 压缩 / L9 压缩中位 | decode 633.30 MB/s · L3 147.10 MB/s (2.71×) · L9 17.75 MB/s |
| `pagesift-office` (`yolowdoc 0.1.1`, B4 81 份) | 进程内吞吐 / 峰值内存 | 208.3 files/s (4.80 ms/file), peak 44.1 MB (median 16.3 MB) |
| `formats` 冒烟（`smoke.py`） | 代码语言 / SQLite UTF-8+16 | 373 种语言（540 扩展名 / 64 文件名，失败 0），SQLite ok=1 |
| B5 OCR medium + 公式 (`GTX 1070`) | olmOCR 总分 / 去 baseline / 速率 | 0.549 / 0.324 (0.10 pages/s) |

### PageSift DataGraph 实测（`pagesift-graph`）

| 工作负载 | 指标 | 实测值 |
|---|---|---|
| A/B 单页 HTML（181 页 AEB，`graph=off` vs `on`） | p50 延迟 / 吞吐 / 峰值 RSS | 0.776 ms → 2.439 ms (1078.0 → 356.0 pages/s), RSS 48.40 → 49.47 MB; 产出 4735 blocks / 17533 citations / 227 provenance / 8325 evidence / 8319 claims |
| `StreamingGraphBuilder` 多来源流式建图（1000 篇） | 建图吞吐 / 总耗时 / RSS | 18458.13 docs/s (ingest 46.195 ms + finalize 7.982 ms), peak RSS 26.52 MB; 1000 nodes / 841 factClusters / 91 conflicts / 250 canonicalClusters |
| E2E 双版本本地站点爬取 + `graph inspect` | 两版合并产出 / 耗时 | v1 4006.32 ms · v2 3987.33 ms; 12 nodes / 16 citations / 6 provenance / 8 versions / 36 evidence / 2 conflicts / 1 canonicalClusters |

## 方法与限制

- 输入、样本、评分规则沿用 `benchmarks/mix/v2/RULES.md` 和各领域 README；1.4 只跑 PageSift 1.4.0，竞品不重跑。
- 实测范围：B1/B3/B4/B5 质量与吞吐（1 路/16 路）、B1 PDF 1,269 页、B4 Office 81 份（`yolowdoc 0.1.1`）、B5 OCR GPU、HTML → Markdown 600 篇、HTTP API、`formats` 冒烟、`yolowsyntax 0.1.2` / `yolowzstd 0.1.2` 独立 crate 路径与 `DataGraph`。
- 已沿用项：本地缺少历史快照或未改动实网的路径（A1/A2/A4/B2、Firecrawl/dates/RAG/crawl/wild/A3/browser/scale）沿用 `benchmarks/history/1.0/results/final/new/metrics.tsv` 记录值；API 测试中的官方 Python SDK 第 7 项因环境未安装 `firecrawl-py` 未测。
- A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。
- A2 的 269 页快照为 2026-10-29 抓取记录，绝对耗时不可与 10-28 旧 A2 直接比较；B1/B4 核心语料一致。
- 每个实测输出文件的 SHA-256 存在 `hashes/`。
- 浏览器、A3、10M scale、Deployment/API、standalone `yolow*` 与 `DataGraph` 开销为无同口径冻结竞品的专项/附录项。
- **缺少指标**：`scale.10m.sustained_pages_per_s`
