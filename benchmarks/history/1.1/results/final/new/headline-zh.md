## 性能一览（PageSift 1.1）

PageSift 1.1 新测；竞品为 2026-10-21/22/27/28 的原始数据，未重跑。每行一个工作负载，PageSift 取该负载的最佳配置；内存单列，不单独进榜。


### 大幅领先

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


### 领先

| 领域 | 工作负载 | 指标 | PageSift 1.0 | 最快对手 | 对手 | 差距 |
|---|---|---|---|---|---|---|
| HTML → Markdown | 600 篇生成文档，保真度 | 平均 F1 | 1.000 | markdownify | 0.936 | +0.064 |
| 难题 PDF | B3 100 份 pdf.js，完成率 | 完成率 | 0.720 | pymupdf4llm | 0.680 | +0.040 |
| 网页正文 | WCXB 测试集 150 页 | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 |



最大差距：fast，40 站 × 60 页，耗时（spider 覆盖的页面约为一半） 16.1 vs spider 1.4 s；A2 回放 269 页，只抓 HTTP、不限速，1 路 47.5 vs spider 156.6 pages/s；A2 随机 269 页，常驻 worker，单页 4.5 vs Resiliparse 2.1 ms/page。完整表见 [报告](report.md)。


A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。
