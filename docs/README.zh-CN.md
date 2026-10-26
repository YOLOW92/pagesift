<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.0

**为 AI 智能体与 LLM 数据工程打造的高性能、严格内存有界抓取与内容提取引擎。**  
把任何网站、学术文档或历史档案转化为干净的 Markdown 与结构化数据——单个纯 Rust 二进制、原生多语言 SDK、兼容 Firecrawl v1/v2 与 Jina Reader 双协议，内存占用明显低于所测竞品（见 [benchmarks.md 第 9 节](benchmarks.md)）。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![2亿合成任务](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](releases/0.70.0.md)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](releases/0.70.0.md)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## 0.80.0 新增内容

**PDF 保留原有结构，结果会说明自己可能漏了什么。**

- **自研 PDF 文件层**替换 lopdf：xref、对象流、解码、加密和坏文件修复都自己做，更快、更省内存。
- **PDF 标题带真实层级**：优先读写入方给的结构树和书签，没有时按编号、字号、粗体推断。AiSeek 报告的 13 份 PDF 上（越高越好）：召回 0.595 → 0.945，精确率 0.400 → 0.872，层级一致率 0.819 → 0.956，空标题 3 → 0。
- **页眉页脚与图表**：识别 `/Artifact` 标记、带页码的页眉页脚和矢量图。olmOCR-bench `headers_footers` 0.681 → 0.734（越高越好）；13 份 PDF 上误判的表格 196 → 18（越低越好）。
- **`POST /v2/parse`**：兼容 Firecrawl 的文件上传解析，支持 PDF、HTML、Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）、EPUB、CSV；`/v2/scrape` 和 `pagesift scrape <文件>` 转换同样的格式。
- **新警告 `partial_content`、`replies_omitted`**：提取的正文可能不完整时提示。在 1,842 个已保存的 WCXB 页面上，丢了正文却没警告的比例 0.842 → 0.667，误报 0.174 → 0.130（都是越低越好）。
- **修复**：批量抓取里同一站点的快页面不再等慢页面；旧版协议的 MCP 客户端在审计日志里有名字了；`pagesift doctor` 不再写审计日志。

详细说明、数据出处和留出集结果见 [0.80.0 发行说明](releases/0.80.0.md)。

## 0.73.0 新增内容

0.73.0 修复三路代码审查与约 150 个奇葩输入测试发现的问题。

- **正文提取重新用满所有核**：0.70.0 起 `scrape`、`crawl`、`serve` 的提取只用一个核；API 服务吞吐约提升 3.5–7 倍（16 并发、`auto` 引擎、同一台 i7-10700 上交替运行：309–336 → 1,153–2,196 请求/秒，p50 延迟 36–45 ms → 4.5–6.1 ms）。
- **修复审查发现的崩溃与 DoS**：feed 判定在中日韩页面上 panic、UTF-8 页面被误纠正成乱码、属性解析平方复杂度（4 万个属性 3 秒 → 0.02 秒）、slowloris 慢速连接、RSS 条目里的 `javascript:` 链接。
- **收紧误伤的启发式**：短篇 Substack 文章曾被判为年龄门；作者组件、cloak 应用根、`<template>` 内容、被当成 OpenAI 密钥的 CSS 变量都已正确处理。
- **API 参数上限与超时**全覆盖，错误一律返回统一的 JSON 格式。
- **crawl 范围看证据决定**（默认 `--scope auto`）：`/docs/python` 只爬 `/docs/python/*`；博客文章这类叶子页种子会放宽到所在栏目，并明确提示。

质量持平或提升（WCXB F1 0.8522 → 0.8533，用 WCXB 官方评分器、2,008 页中的 1,869 页，与下表 D1 行的评分口径不同，不可直接比较；Firecrawl 0.680 → 0.682；AEB 0.958），2,786 个已保存的真实页面上提取快 3–5%（同一台机器、交替运行）。详见 [0.73.0 发行说明](releases/0.73.0.md)。

## 0.72.8 新增内容

0.72.8 完成第四轮现场审计与第五至八轮实测，并加入默认配置发现。

- **字符集声明复核**：HTTP 头或 `<meta>` 说谎时不再静默乱码，而是按正文重新解码，并在 `metadata.pagesift.warnings` 中说明修正原因。
- **`--allow-cidr` 全覆盖**：`crawl`、`map`、`download`、`session-fetch`、`collect`、`serve` 与 `scrape` 共用同一份内网放行名单；非法 CIDR 直接报错。
- **默认配置发现**：`--config` > `PAGESIFT_CONFIG` > 平台配置文件；`PAGESIFT_CONFIG=` 可关闭。`doctor` 会显示实际加载的文件。
- **不再假成功**：`map` 在种子失败时返回错误，`crawl` 零页且有错误时退出码为 1。
- **正文不再误删**：食谱配料表等内容型 `<aside>` 会保留，`quickview`/`teaser` 预览块不再顶替正文。
- **GitHub 代码页**：行号列丢弃，代码行合并进一个代码围栏。
- **作者与元数据**：arXiv 作者行保留，`citation_author` 填入 `metadata.author`；`jsonLd` 原样透传。
- **诚实信号**：德语/法语/西语拦截页归为 `ACCESS_BARRIER`，年龄门为 `interstitial`，过薄抽取给出 `thin_content`，结果多半只有正文一部分时给出 `partial_content` 或 `replies_omitted`。
- **Feed 与媒体**：RSS/Atom 解析成条目列表；图片保留 `title`；视频页用 `VideoObject` 补标题与简介。

详见 [0.72.8 发行说明](releases/0.72.8.md)。

---

## 0.70.0 新增内容

0.70 重构了 HTTP 数据通路：NUMA 本地执行器、精简的 HTTP/1.1 reactor、按源站自适应连接数和有界分片调度。引擎读取 cgroup v2 的 `memory.max` 来约束容量；在 768 MiB 限额下，实测稳定于 380 个连接、817,346 页/秒且没有 OOM。

- **192 vCPU、2 亿合成任务饱和测试**：200,000,000/200,000,000 完成，零失败，947,611 页/秒，211.1 秒，峰值 RSS 888.1 MiB。这是合成负载成绩，不能当作真实公网抓取速度。
- **公开基准连同失利项一起报告**：七个领域中四项领先；正文提取、动态渲染和 DOM 自愈的差距也列在下表。
- **生产问题修复**：请求头等待现在受 `timeout_secs` 约束；429 惩罚可以恢复；单个坏 URL 不再冻结整个源站；低 `ulimit -v` 时避免运行时 panic。

详细设计、测试条件和迁移信息见 [0.70.0 发行说明](releases/0.70.0.md)。

---

## 🏆 七领域公开基准对照（0.70，2026-10 实测，之后的版本未重测）

以下均为**同机全量测试**，使用各基准自己的评分器，并设置 10 GiB 内存上限。测试机为 Intel i7-10700（8C/16T）、15 GB RAM、Ubuntu 26.04。复现脚本见 [`bench/scripts/`](../bench/scripts/)，逐项报告见 [`docs/benchmarks/`](benchmarks/)。

| 领域 | PageSift 0.70 | 最佳对手 | 名次 | 峰值内存 |
| :--- | :--- | :--- | :---: | ---: |
| D1 正文提取（WCXB，2,006 页） | 0.6652 F1 | PageSift 0.50：0.6824 | 第 4 | 751 MiB |
| D3 PDF 提取（olmOCR，1,403 份） | **0.4506** | pymupdf4llm：0.2446 | **第 1** | 271 MiB |
| D5 RAG 分块与 BM25（472 题） | **0.9470 R@5**（自家最佳策略；`recursive` 为 0.9004） | semchunk：0.9364 | **第 1** | — |
| D6 编码检测（3,128 文件） | **0.7602** | chardet：0.7113 | **第 1** | 39 MiB |
| D7 结构化数据（2,006 页） | **0.7392** | bs4+lxml：0.5488 | **第 1** | **21 MiB** |
| D9 动态渲染（7 个 SPA 场景） | 3/7 | Playwright：5/7 | 第 2 | — |
| D4 DOM 自愈（2,000 次变异） | 0.8215 | Scrapling：0.8875 | 第 2 | — |

D1 以较高精确率（0.8938，17 个参赛者中第二，goose3 为 0.9097）换取召回率（0.5851），且 0.70 比 0.50 低 0.017 F1；D9 的 Playwright 耗时（约 2.55 秒）含测试脚本额外加的固定 2 秒等待，不能当作同口径速度对比，且我们没有等到 `setTimeout` 延迟注入的内容；D4 在诱饵场景领先（100% 对 86.4%），但总体排名第二（该数据集“价格”类用例约 14.6% 标注有误，为我们自己的抽查结论）。下方保留旧版本的历史基准，不代表 0.70 的成绩。

---

## ⚔️ 历史数据（0.60，2026-10）：对真实公网发出 1 亿次请求

> **这是一次压力测试，不是“1 亿个真实页面”的成绩。** 历史数据，**当前版本未复测**。出处：
> [`docs/benchmarks/0.60_p4_wild_x_2_showdown.md`](benchmarks/0.60_p4_wild_x_2_showdown.md)；
> 运行器：[`bench/p4/run_wild_x2.py`](../bench/p4/run_wild_x2.py) 与 `bench/p4/src/wild_x2_runner.rs`。原始运行日志不在仓库里。

**设置**：Google Cloud `n4d-highcpu-80`（80 vCPU AMD EPYC 9B45，157 GiB 内存）；Tranco 前 100 万域名与一组固定的常见路径组合，
32 个无共享分片、4,096 路并发连接，**共发出 100,000,000 次请求**。

| 指标 | 结果 |
| :--- | :--- |
| 发出请求数 / 总耗时 | 100,000,000 / 2,373.9 秒（42,124 请求/秒，峰值 > 51,900） |
| **真正返回 `200 OK`** | **314,170 次（0.31%）**；其中 199,422 次提取出 ≥ 100 字节 Markdown（共 2.31 GB） |
| 报告里列出的其他结果 | 403：205,856；DNS 失败：615,232；TLS 失败：60,730；超时：244,243（合计约 144 万次） |
| 报告里未拆分的部分 | 其余约 9,860 万次请求（运行器把它们记在 4xx/5xx 和其他网络错误里） |
| 峰值 RSS | 11.59 GB（上限设为 128 GB），无 OOM |
| 磁盘增量 | < 100 MB（HTML 在内存里解析后丢弃） |

所以这个请求速率既反映提取速度，也反映“失败后快速放弃”的速度：只有约 0.3% 的请求真正拿到了页面。
这次运行能说明的是：1 亿次请求的长跑里，内存和磁盘都在界内、进程没有崩溃。

> **同硬件的合成吞吐（0.60）**：对本机合成源站的 1 亿个**合成**页面，0.60 为 431,412 页/秒（0.50 为 151,483 页/秒），不涉及网络。
> 详见 [`0.60_p5_cross_generation_rebench.md`](benchmarks/0.60_p5_cross_generation_rebench.md)。

---

## 🏆 10 大专项基准矩阵（0.50 历史数据，当前版本未复测）

下表每个数字都取自 `bench/results/final/` 下对应报告的正文实测表（0.50，2026-10-23 前后，单次运行），“出处”列写明报告和章节。报告写明的硬件：D1–D5、10 万对比为 AMD EPYC 9B45（32 核，128 GB）；D6–D10 为 AMD EPYC 7B13（`c2d-standard-32`）。比值只用两个实测数直接相除，并写明是哪两个数。↑ 越高越好，↓ 越低越好。
原表的数字来自 `bench/run_full_10_benchmarks.py` 写死生成的 CSV（脚本不跑任何测试），脚本和这些 CSV 已删除。

| 专项评测领域 | PageSift 0.50（报告实测） | 对手（同一报告实测） | 比值（两个实测数相除） | 峰值内存 ↓（PageSift 对 对手） | 出处 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **01. 正文提取** | WCXB 2,008 页：16 线程 **6,273.9 页/秒** ↑（32 线程 8,833.8）；单线程单页平均 1.30 ms ↓¹ | 报告没有对手的多线程吞吐。单线程单页平均 ↓：trafilatura 2.3.1 26.52 ms，Crawl4AI 0.9.5 283.38 ms | 单页平均耗时 26.52 ÷ 1.30 ≈ 20.4（trafilatura ÷ PageSift） | 报告未测 | [D1](../bench/results/final/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) §2、§4 |
| **02. 10 万真实域名 / 5 万合成 URL** | 10 万：200 OK 62,937 ↑，≥ 100 B Markdown 60,659 ↑，93.5 QPS ↑<br>5 万合成：5,894.8 页/秒 ↑² | Spider 2.53.9 10 万：200 OK 72,209，≥ 100 B Markdown 59,803，666.6 QPS<br>5 万合成：6,532.3 页/秒 | **Spider 更快**：666.6 ÷ 93.5 ≈ 7.1；6,532.3 ÷ 5,894.8 ≈ 1.11<br>内存：8,978.9 ÷ 188.4 ≈ 47.7；11,316.3 ÷ 502.4 ≈ 22.5 | 10 万：188.4 MB 对 8,978.9 MB<br>5 万：502.4 MB 对 11,316.3 MB | [10 万报告](../bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) 总看板；[D2](../bench/results/final/BENCHMARK_D2_MASSIVE_CRAWL_CHAOS_050.md) §2 |
| **03. PDF 解析** | olmOCR-bench 1,269 页：总通过率 **72.4%** ↑，数学公式 70.4% ↑；32 线程 2,682.2 页/秒 ↑³ | Docling 2.142.0：**73.5%（更高）**，0.57 页/秒<br>PyMuPDF4LLM 2.0：64.4%，数学 50.0%，11.1 页/秒 | 吞吐 2,682.2 ÷ 11.1 ≈ 242（对 PyMuPDF4LLM） | 359.6 MB 对 11,663.1 MB（PyMuPDF4LLM） | [D3](../bench/results/final/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) §2、§3 |
| **04. DOM 自愈** | 合成 2,000 例：找回 **98.85%** ↑（1,977/2,000），误报 1.15% ↓，平均 29.2 µs ↓⁴ | Scrapling 0.5：92.95%，误报 7.05%，990.8 µs<br>BS4 + lxml + RapidFuzz：98.85%（持平），2,102.9 µs | 平均耗时 990.8 ÷ 29.2 ≈ 33.9（Scrapling ÷ PageSift） | 63.2 MB 对 1,330.9 MB（Scrapling） | [D4](../bench/results/final/BENCHMARK_D4_DOM_SELF_HEALING_050.md) §2 |
| **05. RAG 分块与过滤** | Chroma 472 题：recursive R@1000 **0.893** ↑，分块 12.89 ms ↓<br>QueryFilter：每题 0.63 ms ↓，保留召回 0.929 ↑ | semchunk：R@1000 0.893，65.60 ms；LangChain：**0.895（更高）**，104.80 ms<br>Crawl4AI BM25ContentFilter：每题 838.00 ms，保留召回 0.673 | 分块 65.60 ÷ 12.89 ≈ 5.1（semchunk）<br>过滤 838.00 ÷ 0.63 ≈ 1,330（Crawl4AI） | 报告未测 | [D5](../bench/results/final/BENCHMARK_D5_RAG_CHUNKING_050.md) §2 |
| **06. 编码探测** | chardet-test-data 3,128 个文件：准确率 **76.02%** ↑；32 线程 24,915.3 文件/秒 ↑⁵ | chardet 7.7.0：71.13%，32 进程 11,412.3 文件/秒 | 吞吐 24,915.3 ÷ 11,412.3 ≈ 2.2 | 不列⁸ | [D6](../bench/results/final/BENCHMARK_D6_CHARSET_DETECTION_050.md) §3.1、§3.2 |
| **07. 结构化数据** | WCXB 2,008 页：33,512.9 页/秒 ↑，p50 0.277 ms ↓；2,000 层嵌套可解析⁶ | extruct 0.18.0：3,449.1 页/秒，p50 2.442 ms；≥ 500 层 `RecursionError` | 吞吐 33,512.9 ÷ 3,449.1 ≈ 9.7 | 不列⁸ | [D7](../bench/results/final/BENCHMARK_D7_STRUCTURED_DATA_050.md) §3.1、§4.1 |
| **08. JS 端点发现** | 合成诱饵集 500 个文件：精确率 **100%** ↑（1,000 个真端点，0 个诱饵）<br>真实 2,000 个 JS 包：160,287.0 文件/秒 ↑⁷ | jsluice 1.0：精确率 40.0%（1,500 个诱饵），7,951.4 文件/秒<br>LinkFinder：精确率 66.7% | 吞吐 160,287.0 ÷ 7,951.4 ≈ 20.2（对 jsluice） | 不列⁸ | [D8](../bench/results/final/BENCHMARK_D8_JS_ENDPOINTS_050.md) §3.1、§4.4 |
| **09. 动态渲染（CDP）** | 本机 5 类 SPA 共 100 次渲染全部成功；**25.7 页/秒** ↑，p50 3,337.56 ms ↓；50 层 Shadow DOM 通过⁹ | Playwright 1.64.1：41.6 页/秒，p50 566.66 ms，Shadow DOM 通过<br>Lightpanda 1.0.2：71.8 页/秒，p50 29.92 ms，Shadow DOM 失败 | **Playwright 更快**：41.6 ÷ 25.7 ≈ 1.6 | 报告未分别测量 | [D9](../bench/results/final/BENCHMARK_D9_DYNAMIC_HEADLESS_050.md) §3.1、§4.2 |
| **10. API 网关** | `/v2/scrape` 1,000 次请求：**308.0 请求/秒** ↑，p50 4.37 ms ↓ | Crawl4AI 网关 50 次请求：33.7 请求/秒，p50 451.03 ms | 308.0 ÷ 33.7 ≈ 9.1（请求数不同，样本小） | 空闲约 20.4 MB 对 约 380 MB（报告标为约数） | [D10](../bench/results/final/BENCHMARK_D10_AI_API_GATEWAYS_050.md) §3.1、§3.4 |

**上表逐行说明（2026-10-27 核对）**
¹ D1 报告 §4 的 16 线程实测为 6,273.9 页/秒；同一行括号里的 3,525 页/秒是 0.40 笔记本的旧数字，原表误用了它。报告 §2 的 F1 0.8472 与 0.70 用官方评分器得到的 WCXB 分数相同、口径未写明，这里不列（WCXB 同机对比见上面的七领域表）。
² 10 万对比为单次运行，双方都跑完；PageSift 处理了 105,660 个 URL，Spider 100,000 个。“96.38% 对 82.82%”是各自 200 OK 页面里提取器返回 ≥ 100 字节 Markdown 的比例，不是保真度。5 万 URL 是本机合成站点（D2 §2）。
³ 报告称“官方全量”，实际是 olmOCR-bench 中原生数字 PDF 的 5 个子集（与 [`benchmarks.md` §5](benchmarks.md#5-pdf-to-markdown) 相同）；报告里对手的总分与 0.12 时的测量完全相同。0.70 同机在 olmOCR-bench 全量 1,403 份 / 7,019 项上为 0.4506 对 pymupdf4llm 0.2446，数学公式 0.3530（[D3](benchmarks/0.70_showdown_D3.md)）。
⁴ 合成变异集。0.70 同机另一个 2,000 例集上 PageSift 0.8215、Scrapling 0.8875，0.50 为 0.6965（[D4](benchmarks/0.70_showdown_D4.md)），与本行结论相反。
⁵ Unicode 以外的四个语系 chardet 都更准（如西欧语系 71.62% 对 50.00%，D6 §3.2）。0.70 同机重测准确率相同（0.7602 对 0.7113），速度 11,239 对 6,800 文件/秒（[D6](benchmarks/0.70_showdown_D6.md)）。
⁶ 只统计抽到的实体数，没有标准答案；准确率见 0.70 同机 [D7](benchmarks/0.70_showdown_D7.md)（0.7392 对 bs4+lxml 0.5488）。
⁷ 真实 2,000 个包上 PageSift 只报出 400 个端点，jsluice 1,041 个、LinkFinder 2,840 个；没有标准答案，无法判断召回。
⁸ D6–D8 报告里所有工具（包括 32 进程的对手）峰值内存都只有约 20–32 MB，像是只测了主进程，口径存疑，不列。
⁹ 0.70 同机 7 个 SPA 场景：PageSift 3/7、Playwright 5/7（[D9](benchmarks/0.70_showdown_D9.md)）。

> [!TIP]
> 各报告的原始 JSON/JSONL（如 `benchmark_d6_results.json`、`benchmark4_tier1_standard_2000.jsonl`）在 [`bench/results/final/`](../bench/results/final/)；其中个别数字与报告正文有出入（如 D3、D9 的吞吐），上表以报告正文为准。

---

## 0.50.0 全新特性

- 🧠 **动态自适应并发与超时引擎 (`pagesift::adaptive`)**：
  - **木桶短板原则 ($\min$ 原则)**：并发度严格受 CPU 算力、物理空闲内存（每连接预留 2MB）以及操作系统句柄限制（`ulimit -n`）的三者交集硬约束：
    $$\text{Concurrency} = \min\big(C_{cpu},\; M_{mem},\; FD_{safe}\big)$$
  - **全场景硬件规格自动识别**：覆盖 Micro（1C1GB）、Small（2C2GB~4C4GB）、Mid Desktop（8C16GB / i7-10700）、High Server（32C128GB / AMD EPYC）与 Extreme Supernode（64C+ 128GB+ FD>=65k）。
  - **双模式运行开关**：支持 `--tuning-mode adaptive`（自适应满载吞吐）与 `--tuning-mode conservative`（严格压制 CPU < 20%，适合个人办公机后台静默运行）。
  - **跨国网络长 RTT 保活**：将基准连接超时精准校准至 3.5s、总超时 10.0s，彻底杜绝激进超时对欧美、亚太等跨国慢速站点的误杀。
- 🌐 **Firecrawl v1 & v2 双协议完全兼容**：官方 Python / Node SDK 零修改无缝接入，覆盖 `/v1/scrape`、`/v1/crawl`、`/v1/batch/scrape`、`/v1/map` 及 `/v2/*` 全量端点。
- ⚡ **Jina Reader 极简流式输出**：支持极简 `GET /r/{*url}` 与 `POST /r`，支持 `Accept: text/event-stream` SSE 实时流式直出 Markdown，为各类 LLM Agent 提供即插即用体验。
- 🛡️ **128 GB 生产服务器 10 万实测零泄漏**：实测 105,660 个真实公网站点物理内存始终锁死在 **188.4 MB**，同场对比中 Spider 峰值为 8,978.9 MB（报告原文称“泄漏”，未做泄漏分析）；Spider 的请求吞吐也更高（666.6 对 93.5 QPS）。
- 🦀 **几乎全部为安全 Rust**：除 `pagesift-sys`（一处 glibc `mallopt` 调用）外，每个 crate 都声明了 `#![forbid(unsafe_code)]`。（旧版文字称“所有源文件不超过 500 行”，现状并非如此，已删除。）

---

## 快速开始

### 1. Docker / 本地自托管服务

启动独立容器（内置轻量无头浏览器渲染）：

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.0
```

#### Firecrawl SDK 零改动平替接入
现有使用 Firecrawl 的项目直接修改连接配置即可生效：

```python
from firecrawl import FirecrawlApp

# 零改动接入本地 PageSift 服务
app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### 上传文件解析（`/v2/parse`）
本地或不公开的 PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV 文件不用先放到网址上，直接上传解析；兼容 Firecrawl 的 multipart 写法，上限 50 MB，返回与 `/v2/scrape` 相同（[说明](parse-api.md)）。Office、EPUB、CSV 的转换是 pagesift 自己用 Rust 写的（ZIP 和 XML 读取也是自己的）；命令行 `pagesift scrape report.docx` 效果相同：

```bash
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'
```

#### Jina Reader 极简平替接入
任何 LLM 智能体或终端命令行直接请求 `/r/` 端点：

```bash
# 直接获取清洁 Markdown 正文
curl -s http://localhost:3002/r/https://en.wikipedia.org/wiki/Rust_(programming_language)

# 实时 SSE 流式输出
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Python SDK

```bash
pip install pagesift
```

```python
import pagesift

# 提取本身约 1–2 ms（中位数，不含网络）
page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

# 动态自适应多任务并发抓取
results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Node.js / TypeScript SDK

```bash
npm install @pagesift/node
# 或者
npm install pagesift
```

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

// 提取清洁 LLM Markdown
const page = await scrape('https://example.com', {
  formats: ['markdown', 'links']
})
console.log(page.markdown)

// 并发流式处理
const stream = scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })
for await (const page of stream) {
  console.log(`Scraped: ${page.url} in ${page.elapsedMs}ms`)
}
```

---

### 4. 命令行工具 (CLI)

```bash
# Cargo 原生安装
cargo install pagesift --locked

# 单 URL 抓取
pagesift scrape "https://news.ycombinator.com" --format markdown

# 动态自适应高并发深度全网抓取
pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --out ./data/
```

### 5. 给 AI 智能体共用的 MCP 服务

```bash
# 本机所有智能体共用一个服务（只监听 127.0.0.1）
pagesift mcp http --port 3003 --audit-log ~/.local/state/pagesift/audit.jsonl
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

共用浏览器和缓存，每次抓取写一行审计日志，可疑结果保存 HTML 快照，并提供 `pagesift_report_issue`
工具。详见 [Running pagesift as a shared MCP service](mcp-service.md)。

---

## 为什么选择 PageSift？（新旧版本数字混排，见表下注释）

| 核心特性 | PageSift（版本与出处见注释） | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **底层架构** | Rust；除 `pagesift-sys`（一处 glibc `mallopt` 调用）外每个 crate 都声明 `forbid(unsafe_code)` | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API 兼容度** | **Firecrawl v1+v2 + Jina Reader + MCP** | 仅 Firecrawl API | 自定义 Python API | 自定义 REST API |
| **正文提取吞吐 ↑** | 6,273.9 页/秒（0.50，16 线程，WCXB 2,008 页）ᵃ；本机合成源站 59,309（16 vCPU）/ 947,611（192 vCPU）ᵇ | 未实测 | 未实测；单线程单页平均 283.38 ms ↓（PageSift 1.30 ms）ᵃ | 未实测 |
| **十万域名峰值内存 ↓** | **188.4 MB**（0.50）ᶜ | 未实测 | 未实测 | 8,978.9 MB（同场 Spider 吞吐更高，666.6 对 93.5 QPS）ᶜ |
| **200 OK 页面中产出 ≥ 100 字节 Markdown 的比例 ↑** | 96.38%（0.50）ᶜ | 未实测 | 未实测 | 82.82% ᶜ |
| **硬件自适应引擎** | **$\min(CPU, RAM, FD)$ 全自动** | 人工配置并发数 | 人工配置信号量 | 固定信号量 |
| **DOM 智能自愈（找回率 ↑）** | 2,000 例集 0.8215（0.70，低于 Scrapling 的 0.8875）；0.50 报告的另一个合成 2,000 例集为 98.85% ᵈ | ❌ 不支持 | ❌ 不支持 | ❌ 不支持 |
| **PDF 解析（通过率 ↑）** | olmOCR-bench 全量 0.4506（0.70，pymupdf4llm 0.2446）；0.50 报告 5 个子集 72.4%（Docling 73.5% 更高）ᵈ | 外部 docx/pdf 文本 | 仅粗糙纯文本 | 仅粗糙纯文本 |
| **RAG 分块与过滤（耗时 ↓）** | 分块 12.89 ms / QueryFilter 每题 0.63 ms（0.50，Chroma 472 题）ᵉ | ❌ 不支持 | BM25 过滤每题 838.00 ms ᵉ | ❌ 不支持 |
| **反爬治理哲学** | 规范守序、透明 UA、自适应礼貌队列 | 代理轮询 / 绕过 | 浏览器伪装反反爬 | 浏览器伪装反反爬 |

ᵃ [`BENCHMARK_D1_CONTENT_EXTRACTION_050.md`](../bench/results/final/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) §2、§4（0.50，单次运行）。
ᵇ 合成源站、同机、无网络（[`0.70_200m_regression.md`](benchmarks/0.70_200m_regression.md)），与其他格不可比。
ᶜ [`BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](../bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md)（0.50，单次运行）。百分比是各自提取器返回 ≥ 100 字节的页面占比，不是保真度。
ᵈ 0.70 的数字是较新的同机运行（[D4](benchmarks/0.70_showdown_D4.md)、[D3](benchmarks/0.70_showdown_D3.md)）；0.50 的出自 [D4 报告](../bench/results/final/BENCHMARK_D4_DOM_SELF_HEALING_050.md) §2 和 [D3 报告](../bench/results/final/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) §2，数据集不同，结论也不一致。
ᵉ [`BENCHMARK_D5_RAG_CHUNKING_050.md`](../bench/results/final/BENCHMARK_D5_RAG_CHUNKING_050.md) §2（0.50）。
“未实测”：仓库里没有这一格的测量记录。旧版表格这些格里的“~150 / ~90 / ~1,200 页/秒”“> 8 GB / > 10 GB”“~85% / ~88%”是没有出处的估计，已删除。

---

## 架构核心优势

- **分段有界前沿队列 (Segmented Bounded Frontier)**：队列有界，内存不随抓取深度增长（0.50 的 10 万域名实测峰值 188.4 MB）。
- **动态硬件自适应调优 (Adaptive Resource Engine)**：毫秒级自动侦测系统物理短板，科学压榨机器极限而不踩死锁或爆 FD。
- **选择性渲染管线 (Selective Rendering Pipeline)**：智能预判页面动静形态，仅当客户端水合带来显著正文增益时才调用无头 Chromium。
- **SSRF 深度沙箱防护**：内置严格私有 IP 段过滤、DNS-Rebinding 免疫防线，默认拦截私有网段目标（设计目标，有测试覆盖，未经第三方审计）。
- **RFC 规范自适应礼貌机制**：单域名自适应令牌桶退避、全自动 robots.txt 解析与合规声明。

---

## 社区

有问题或反馈，欢迎加入 [Discord](https://discord.gg/esA3wMZVvA)。即将开放的托管 API 内测（尚未上线）也会在那里发布消息。

---

## 开源协议与商业授权

- **开源版本**：基于 [AGPL-3.0](LICENSE) 许可协议开源。
- **商业授权**：如需闭源商业集成、免除 AGPL 开源义务或获取企业级技术支持，欢迎阅读 [商业授权指引](COMMERCIAL.md) 或联系 `licensing@pagesift.dev`。
