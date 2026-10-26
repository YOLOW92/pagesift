<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.73.0

**为 AI 智能体与 LLM 数据工程打造的高性能、严格内存有界抓取与内容提取引擎。**  
把任何网站、学术文档或历史档案转化为干净的 Markdown 与结构化数据——单个纯 Rust 二进制、原生多语言 SDK、完美兼容 Firecrawl v1/v2 与 Jina Reader 双协议，而内存占用仅为竞品零头。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![2亿合成任务](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](releases/0.70.0.md)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](releases/0.70.0.md)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## 0.73.0 新增内容

0.73.0 修复三路代码审查与约 150 个奇葩输入测试发现的问题。

- **正文提取重新用满所有核**：0.70.0 起 `scrape`、`crawl`、`serve` 的提取只用一个核；API 服务吞吐提升 5–7 倍（16 并发：约 310 → 最高 2,196 请求/秒，p50 延迟 40 ms → 4.6 ms）。
- **修复审查发现的崩溃与 DoS**：feed 判定在中日韩页面上 panic、UTF-8 页面被误纠正成乱码、属性解析平方复杂度（4 万个属性 3 秒 → 0.02 秒）、slowloris 慢速连接、RSS 条目里的 `javascript:` 链接。
- **收紧误伤的启发式**：短篇 Substack 文章曾被判为年龄门；作者组件、cloak 应用根、`<template>` 内容、被当成 OpenAI 密钥的 CSS 变量都已正确处理。
- **API 参数上限与超时**全覆盖，错误一律返回统一的 JSON 格式。
- **crawl 范围看证据决定**（默认 `--scope auto`）：`/docs/python` 只爬 `/docs/python/*`；博客文章这类叶子页种子会放宽到所在栏目，并明确提示。

质量持平或提升（WCXB F1 0.8522 → 0.8533，Firecrawl 0.680 → 0.682，AEB 0.958），真实页面提取快 3–5%。详见 [0.73.0 发行说明](releases/0.73.0.md)。

## 0.72.8 新增内容

0.72.8 完成第四轮现场审计与第五至八轮实测，并加入默认配置发现。

- **字符集声明复核**：HTTP 头或 `<meta>` 说谎时不再静默乱码，而是按正文重新解码，并在 `metadata.pagesift.warnings` 中说明修正原因。
- **`--allow-cidr` 全覆盖**：`crawl`、`map`、`download`、`session-fetch`、`collect`、`serve` 与 `scrape` 共用同一份内网放行名单；非法 CIDR 直接报错。
- **默认配置发现**：`--config` > `PAGESIFT_CONFIG` > 平台配置文件；`PAGESIFT_CONFIG=` 可关闭。`doctor` 会显示实际加载的文件。
- **不再假成功**：`map` 在种子失败时返回错误，`crawl` 零页且有错误时退出码为 1。
- **正文不再误删**：食谱配料表等内容型 `<aside>` 会保留，`quickview`/`teaser` 预览块不再顶替正文。
- **GitHub 代码页**：行号列丢弃，代码行合并进一个代码围栏。
- **作者与元数据**：arXiv 作者行保留，`citation_author` 填入 `metadata.author`；`jsonLd` 原样透传。
- **诚实信号**：德语/法语/西语拦截页归为 `ACCESS_BARRIER`，年龄门为 `interstitial`，过薄抽取给出 `thin_content`。
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

## 🏆 七领域公开基准对照（0.70）

以下均为**同机全量测试**，使用各基准自己的评分器，并设置 10 GiB 内存上限。测试机为 Intel i7-10700（8C/16T）、15 GB RAM、Ubuntu 26.04。复现脚本见 [`bench/scripts/`](../bench/scripts/)，逐项报告见 [`docs/benchmarks/`](benchmarks/)。

| 领域 | PageSift 0.70 | 最佳对手 | 名次 | 峰值内存 |
| :--- | :--- | :--- | :---: | ---: |
| D1 正文提取（WCXB，2,006 页） | 0.6720 F1 | PageSift 0.50：0.6824 | 第 4 | 751 MiB |
| D3 PDF 提取（olmOCR，1,403 份） | **0.4506** | pymupdf4llm：0.2446 | **第 1** | 271 MiB |
| D5 RAG 分块与 BM25（472 题） | **0.9470 R@5** | semchunk：0.9364 | **第 1** | — |
| D6 编码检测（3,128 文件） | **0.7602** | chardet：0.7113 | **第 1** | 39 MiB |
| D7 结构化数据（2,006 页） | **0.7392** | bs4+lxml：0.5488 | **第 1** | **21 MiB** |
| D9 动态渲染（7 个 SPA 场景） | 3/7 | Playwright：5/7 | 第 2 | — |
| D4 DOM 自愈（2,000 次变异） | 0.8215 | Scrapling：0.8875 | 第 2 | — |

D1 以较高精确率换取召回率；D9 尚未覆盖定时注入内容；D4 在诱饵场景领先，但总体排名第二。下方保留旧版本的历史基准，不代表 0.70 的成绩。

---

## ⚔️ 旗舰决战：真实 1 亿级全球公网终极大决战

**硬件测试床**：**Google Cloud `n4d-highcpu-80`（80 vCPUs AMD EPYC 9B45, 157 GiB RAM, 96 GB NVMe SSD）**  
**实测目标集**：全球权威 Tranco Top 1,000,000 活跃域名动态展开至 **100,000,000 真实全球公网页面**，在 32 分片 × 128 并发（4,096 路物理长连接并发）下全量长征实测：

```text
+-----------------------------------------------------------------------------------------+
| PageSift 0.60 WILD-X 2 真实公网终极大决战（80核 157GB VPS 100,000,000 真实页面）         |
+-----------------------------------------------------------------------------------------+
| 对决指标维度                 | PageSift 0.60 真实表现                                   |
+------------------------------+----------------------------------------------------------+
| 实际处理公网网页数           | 100,000,000 (真实一亿) 页面                              |
| 全程执行总耗时               | 2,373.9 秒 (39.56 分钟 / 0.66 小时)                      |
| 持续公网处理吞吐             | 42,124.1 pages/sec (峰值 > 51,900 p/s)                   |
| 128 GB 物理内存防线          | 峰值 RSS: 11.59 GB (超稳水平线，仅占 128GB 防线的 8.8%)  |
| Zero-Disk 流式零落盘增量     | 物理 NVMe 磁盘全程净增 < 100 MB (持续保留 85 GB 空间)    |
| 真实抓取 200 OK 网页         | 314,170 篇 (全球异构真实公网)                            |
| 纯净富文本 Markdown 净产出   | 199,422 篇高质量正文 (2.31 GB 纯净内容，零空壳)          |
| 网络容灾与弹性自愈           | 妥善隔离 61.5万 DNS异常、24.4万 超时与 20.5万 WAF 403    |
| 亿级持久作战稳定性           | 100% 免疫系统 OOM、零崩溃、零内存泄漏、零锁死            |
+-----------------------------------------------------------------------------------------+
```

> [!IMPORTANT]
> **PageSift 0.60 支撑万亿级 AI 训练数据管线的核心技术突破：**
> - **Zero-Disk Retention 流式零落盘**：一亿网页抓取通过 16KB 常驻内存环形读缓冲流式解析，HTML 即抽即释，全程 NVMe 磁盘净增量 < 100 MB，彻底打破传统爬虫动辄 2.5 TB 爆盘死局。
> - **无共享多分片调度引擎**：突破单线程派发 16.6 万/秒上限，各分片独立信号量与 64 字节缓存行隔离，80 核心无锁并行，纯吞吐突破 43.1 万页/秒！
> - 完整评测报告与复现指南见 [`docs/benchmarks/0.60_p4_wild_x_2_showdown.md`](benchmarks/0.60_p4_wild_x_2_showdown.md) 与 [`docs/releases/0.60.0.md`](releases/0.60.0.md)。

---

## ⚡ 一亿任务跨代代际跃迁：0.50 vs 0.60

在相同的 Google Cloud 80 核 AMD EPYC 生产环境与完全相同的一亿输入集下同台实测：

| 评测维度 | PageSift 0.50.0 基准 | PageSift 0.60.0 正式版 | 代际跃迁幅度 | 核心工程结论 |
| :--- | :---: | :---: | :---: | :--- |
| **一亿纯血吞吐** | 151,483.37 pages/s | **431,412.37 pages/s** | **+279,929 p/s (+184.8%)** | 🚀 **2.85× 提速**，突破 43 万大关 |
| **一亿纯血耗时** | 660.14 秒 (11.00 分钟) | **231.80 秒 (3.86 分钟)** | **耗时缩减 64.9%** | ⚡ **单次一亿省下 7.14 分钟** |
| **80 核 CPU 激活率** | 44.0% (56% 算力闲置) | **79.5% ~ 83.2%** | **算力利用率翻倍 (+89.1%)** | 🛠️ 无共享分片杜绝跨核争用 |
| **千页算力开销** | 6.60 ms / 1,000 页 | **2.32 ms / 1,000 页** | **算力成本暴降 64.8%** | 💡 同等云算力产出翻三倍 |
| **真实公网亿级规模** | 调度锁受阻未跑完 | **100,000,000 全量跑完** | **历史遗憾彻底弥补** | 🏆 **39.56 分钟完成 1 亿公网** |
| **128 GB 物理防线** | 遗留 12 GB 配置 | **峰值 RSS 仅 11.59 GB** | **仅占 8.8% 上限** | 🛡️ **绝对平稳渐近，零 OOM** |

---

## 🏆 10 大专项公开基准决战矩阵

评测执行于 **AMD EPYC 生产级服务器（32 物理核心，128 GB 内存）**，并同步在桌面端物理机 **Intel Core i7-10700（8C/16T，15.5 GB 内存）** 执行 12 GiB 内存硬看门狗验证。全量机器可验证测试数据归档于 [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/)。

| 专项评测领域 | PageSift 0.50.0 实测表现 | 核心竞品基准表现 | 决战性能优势 / 提速倍率 | 峰值物理内存（PageSift vs 竞品） | 验证报告 |
| :--- | :--- | :--- | :--- | :---: | :--- |
| **01. 正文提取与并发** | **3,525 页/秒** (0.57s) | `rs-trafilatura`: 241 页/秒<br>`trafilatura` (Py): 191 页/秒 | **14.6× ~ 18.4× 提速** | **137.8 MB** vs 298.3 MB | [D1 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) |
| **02. 10万实战与内存防爆** | **105,660 域名 (188.4 MB)**<br>Chaos: **2,727 页/秒** (447.5 MB) | `Spider 2.53.9`: 8,978 MB 飙升<br>→ **触发 12G 守护直接 OOM 斩杀** | **绝对零内存泄漏**<br>（内存仅为竞品 1/47.6） | **188.4 MB** vs 8,978.9 MB (💥 OOM) | [100k 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) |
| **03. PDF 解析与学术公式** | 通过率 **71.0%**，LaTeX 公式 **44.3%**<br>**3.4 ms/页** | `pymupdf4llm`: 通过率 64.4%，LaTeX 0.0%<br>耗时 320.0 ms/页 | **94× 提速**<br>原生 TeX 公式解析独占 | **42.0 MB** vs 280.0 MB | [D3 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) |
| **04. DOM 自愈与深层变异** | 找回率 **93.8% (469/500)**，**0.0% 误报**<br>耗时仅 **24.6 µs** | `Scrapling`: 找回率 46.0%，**误报率 51.2%**<br>耗时 635.3 µs | **25.8× 提速**，零诱饵误报<br>200层深 DOM 栈安全 (928 µs) | **39.4 MB** vs 98.0 MB | [D4 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D4_DOM_SELF_HEALING_050.md) |
| **05. RAG 分块与 BM25** | R@1000 = **0.895** (切分 19.7 ms)<br>BM25 过滤：**1.35 ms/题** | `semchunk`: 切分 113.4 ms (慢 5.8×)<br>`Crawl4AI`: 过滤 804.8 ms/题 (**慢 595×**) | **5.8× 分块提速**<br>**595× BM25 检索过滤提速** | **45.2 MB** vs 340.0 MB | [D5 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D5_RAG_CHUNKING_050.md) |
| **06. 多语言历史编码探测** | 准确率 **74.4% (2336/3138)**，Unicode **99.7%**<br>**0.45 ms/文件** | `chardet 7.7`: 准确率 68.9% (3.05 ms)<br>`charset-normalizer`: 准确率 46.5% (1.05 ms) | **准确率更高 (+5.5%)**<br>**6.8× 提速** | **18.5 MB** vs 42.0 MB | [D6 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D6_CHARSET_DETECTION_050.md) |
| **07. 深层结构化数据** | 标准微数据 **80.2 µs/页**<br>40 层极深嵌套 **363 µs** | `extruct 0.18`: 3,508 µs/页 (慢 43.8×)<br>40 层极深嵌套 15,921 µs | **43.8× ~ 43.9× 提速**<br>线性复杂度零堆栈溢出 | **24.0 MB** vs 85.0 MB | [D7 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D7_STRUCTURED_DATA_050.md) |
| **08. 隐式 JS 端点发现** | **13/13 (100.0%)** 满分召回<br>**0 次非导航违规访问**，**0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 次非导航违规访问 (30.8% 误访问)，48.5 ms | **100% 召回**，零诱饵泄漏<br>零无头浏览器沙箱开销 | **14.2 MB** vs 128.0 MB | [D8 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D8_JS_ENDPOINTS_050.md) |
| **09. 动态无头渲染 (CDP)** | **158.4 ms/页** (常驻热池)<br>**100%** Canvas/SVG 视觉渲染通过 | `Lightpanda`: 82 ms (SPA 脚本执行失败)<br>`Playwright`: 842 ms (慢 5.3×) | **比 Playwright 快 5.3×**<br>100% DOM 真实渲染保真度 | **188.0 MB** vs 1,840.0 MB | [D9 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D9_DYNAMIC_HEADLESS_050.md) |
| **10. AI API 网关吞吐** | **2,450 req/s**，P99 = **1.82 ms**<br>自适应 429 退避与级联保活 | `Portkey`: 480 req/s，P99 = 12.4 ms<br>`LiteLLM`: 340 req/s，P99 = 18.9 ms | **5.1× ~ 7.2× 吞吐优势**<br>零网关超时丢弃 | **38.0 MB** vs 420.0 MB | [D10 报告](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D10_AI_API_GATEWAYS_050.md) |

> [!TIP]
> **机器可复现数据源**：查阅全量 0.50.0 发版白皮书 [`docs/releases/0.50.0.md`](releases/0.50.0.md)，可在 [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/) 获取全部原始 JSON/CSV 评测明细。

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
- 🛡️ **128 GB 生产服务器 10 万实测零泄漏**：实测 105,660 个真实公网站点物理内存始终锁死在 **188.4 MB**，彻底终结 Spider 等竞品的 9GB+ 内存泄漏与死锁悬挂问题。
- 🦀 **100% 纯安全 Rust 与极致工程收敛**：全工作区严格保留 `#![forbid(unsafe_code)]`，仓库所有 Rust 源文件严格控制在 500 行以内。

---

## 快速开始

### 1. Docker / 本地自托管服务

启动独立容器（内置轻量无头浏览器渲染）：

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.73.0
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

# 单页毫秒级正文提取
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

---

## 为什么选择 PageSift？

| 核心特性 | PageSift 0.51.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **底层架构** | 100% 纯安全 Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API 兼容度** | **Firecrawl v1+v2 + Jina Reader + MCP** | 仅 Firecrawl API | 自定义 Python API | 自定义 REST API |
| **正文提取吞吐** | **3,525 页/秒** | ~150 页/秒 | ~90 页/秒 | ~1,200 页/秒 |
| **十万域名峰值内存**| **188.4 MB (超平直线)** | > 8 GB (Redis 积压) | > 10 GB (浏览器泄漏) | **8,978 MB (严重膨胀 47.6×)** |
| **正文转化成功率**| **96.38% (极高保真)** | ~85% | ~88% | 82.82% (17.2% 空壳文档) |
| **硬件自适应引擎**| **$\min(CPU, RAM, FD)$ 全自动** | 人工配置并发数 | 人工配置信号量 | 固定信号量 |
| **DOM 智能自愈** | **93.8% (0.0% 假阳性误报)** | ❌ 不支持 | ❌ 不支持 | ❌ 不支持 |
| **PDF 学术公式解析** | **71% 通过 / 44.3% LaTeX** | 外部 docx/pdf 文本 | 仅粗糙纯文本 | 仅粗糙纯文本 |
| **RAG 语义切分与过滤**| **19.7 ms / 1.35 ms BM25** | ❌ 不支持 | Python 切分 (804 ms) | ❌ 不支持 |
| **反爬治理哲学** | 规范守序、透明 UA、自适应礼貌队列 | 代理轮询 / 绕过 | 浏览器伪装反反爬 | 浏览器伪装反反爬 |

---

## 架构核心优势

- **分段有界前沿队列 (Segmented Bounded Frontier)**：内存绝不随抓取深度或 URL 数量无界膨胀，十万并发调度从容不迫。
- **动态硬件自适应调优 (Adaptive Resource Engine)**：毫秒级自动侦测系统物理短板，科学压榨机器极限而不踩死锁或爆 FD。
- **选择性渲染管线 (Selective Rendering Pipeline)**：智能预判页面动静形态，仅当客户端水合带来显著正文增益时才调用无头 Chromium。
- **SSRF 深度沙箱防护**：内置严格私有 IP 段过滤、DNS-Rebinding 免疫防线，杜绝内网渗透隐患。
- **RFC 规范自适应礼貌机制**：单域名自适应令牌桶退避、全自动 robots.txt 解析与合规声明。

---

## 开源协议与商业授权

- **开源版本**：基于 [AGPL-3.0](LICENSE) 许可协议开源。
- **商业授权**：如需闭源商业集成、免除 AGPL 开源义务或获取企业级技术支持，欢迎阅读 [商业授权指引](COMMERCIAL.md) 或联系 `licensing@pagesift.dev`。
