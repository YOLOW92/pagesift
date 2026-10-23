<div align="center">

<img src="../assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.50.0

**为 AI 智能体与 LLM 数据工程打造的高性能、严格内存有界抓取与内容提取引擎。**  
把任何网站、学术文档或历史档案转化为干净的 Markdown 与结构化数据——单个纯 Rust 二进制、原生多语言 SDK、完美兼容 Firecrawl v1/v2 与 Jina Reader 双协议，而内存占用仅为竞品零头。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![12GiB 内存防爆认证](https://img.shields.io/badge/12GiB%20%E5%86%85%E5%AD%98%E9%98%B2%E5%BE%A1-%E5%AE%9E%E6%B5%8B%20OOM%20%E5%85%8D%E7%96%AB-success.svg)](#-10-大专项公开基准决战)
[![10万真实全网实测](https://img.shields.io/badge/10%E4%B8%87%E7%BD%91%E7%AB%99%E5%AE%9E%E6%B5%8B-47.6x%20%E5%86%85%E5%AD%98%E4%BC%98%E5%8A%BF-blue.svg)](#-10-万真实全网顶级域名世纪决战pagesift-vs-spider-2539)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## 🏆 10 大专项公开基准决战（数据全面对比）

全部评测均在 AMD EPYC（32 物理核心，128 GB 内存）与物理机 **Intel Core i7-10700**（8 物理核心，16 逻辑超线程，15.5 GB 物理内存）下同台执行，系统常驻 **12 GiB 内存硬防御看门狗**（`MemoryMax=12288M`）。完整可验证原始数据集与复现脚本归档于 [`bench/results/final/`](../bench/results/final/)。

| 专项评测领域 | PageSift 0.50.0 实测表现 | 核心竞品基准表现 | 决战性能优势 / 提速倍率 | 峰值物理内存（PageSift vs 竞品） |
| :--- | :--- | :--- | :--- | :---: |
| **01. 正文提取与并发** | **3,525 页/秒** (0.57s) | `rs-trafilatura`: 241 页/秒 (8.32s)<br>`trafilatura` (Py): 191 页/秒 (10.51s) | **14.6× ~ 18.4× 提速** | **137.8 MB** vs 298.3 MB |
| **02. 5万页爬取与 12G 防爆** | **2,727 页/秒** (17.76s)<br>峰值仅 **447.5 MB** | `Spider 2.53.9`: 内存飙升至 11.7 GiB<br>→ **触发 12G 守护直接 OOM 斩杀** | **绝对零内存泄漏**<br>（内存仅为竞品 1/26） | **447.5 MB** vs 11,724.8 MB (💥 OOM 崩溃) |
| **03. PDF 解析与学术公式** | 通过率 **71.0%**，LaTeX 公式 **44.3%**<br>**3.4 ms/页** | `pymupdf4llm`: 通过率 64.4%，LaTeX 0.0%<br>耗时 320.0 ms/页 | **94× 提速**<br>原生 TeX 公式解析独占 | **42.0 MB** vs 280.0 MB |
| **04. DOM 自愈与深层变异** | 找回率 **93.8% (469/500)**，**0.0% 误报**<br>耗时仅 **24.6 µs** | `Scrapling`: 找回率 46.0%，**误报率 51.2%**<br>耗时 635.3 µs | **25.8× 提速**，零诱饵误报<br>200层深 DOM 栈安全 (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. RAG 分块与 BM25** | R@1000 = **0.895** (切分 19.7 ms)<br>BM25 过滤：**1.35 ms/题** | `semchunk`: 切分 113.4 ms (慢 5.8×)<br>`Crawl4AI`: 过滤 804.8 ms/题 (**慢 595×**) | **5.8× 分块提速**<br>**595× BM25 检索过滤提速** | **45.2 MB** vs 340.0 MB |
| **06. 多语言历史编码探测** | 准确率 **74.4% (2336/3138)**，Unicode **99.7%**<br>**0.45 ms/文件** | `chardet 7.7`: 准确率 68.9% (3.05 ms)<br>`charset-normalizer`: 准确率 46.5% (1.05 ms) | **准确率更高 (+5.5%)**<br>**6.8× 提速** | **18.5 MB** vs 42.0 MB |
| **07. 深层结构化数据** | 标准微数据 **80.2 µs/页**<br>40 层极深嵌套 **363 µs** | `extruct 0.18`: 3,508 µs/页 (慢 43.8×)<br>40 层极深嵌套 15,921 µs | **43.8× ~ 43.9× 提速**<br>线性复杂度零堆栈溢出 | **24.0 MB** vs 85.0 MB |
| **08. 隐式 JS 端点发现** | **13/13 (100.0%)** 满分召回<br>**0 次非导航违规访问**，**0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 次非导航违规访问 (30.8% 误访问)，48.5 ms | **100% 召回**，零诱饵泄漏<br>零无头浏览器沙箱开销 | **14.2 MB** vs 128.0 MB |
| **09. 动态无头渲染 (CDP)** | **158.4 ms/页** (常驻热池)<br>**100%** Canvas/SVG 视觉渲染通过 | `Lightpanda`: 82 ms (SPA 脚本执行失败)<br>`Playwright`: 842 ms (慢 5.3×) | **比 Playwright 快 5.3×**<br>100% DOM 真实渲染保真度 | **188.0 MB** vs 1,840.0 MB |
| **10. AI API 网关吞吐** | **2,450 req/s**，P99 = **1.82 ms**<br>自适应 429 退避与级联保活 | `Portkey`: 480 req/s，P99 = 12.4 ms<br>`LiteLLM`: 340 req/s，P99 = 18.9 ms | **5.1× ~ 7.2× 吞吐优势**<br>零网关超时丢弃 | **38.0 MB** vs 420.0 MB |

---

### ⚔️ 10 万真实全网顶级域名世纪决战：PageSift vs. Spider 2.53.9

在 32 核 AMD EPYC 服务器同台实战，针对全球 Tranco Top 100,000 活跃网站，在 768 高并发与真实公网长 RTT 环境下：

```text
+-------------------------------------------------------------------------------+
| PageSift 0.50 vs Spider 2.53.9 (100,000 真实公网域名同台决战)                 |
+-------------------------------------------------------------------------------+
| 指标项                      | PageSift 0.50 (Rust)   | Spider 2.53.9 (Rust)   |
+-----------------------------+------------------------+------------------------+
| 实际处理网站数              | 105,660 URLs           | 100,000 URLs           |
| 真实 200 OK 响应数          | 62,937 页面            | 72,209 页面 (含报错页) |
| 有效 Markdown 正文 (>100B)  | 60,659 份文档          | 59,803 份文档          |
| 正文提取成功转化率          | 96.38% (优质正文)      | 82.82% (17.2% 空壳文档)|
| 内容 Blake3 精确去重        | 2,449 篇重复页面       | 3,484 篇重复页面       |
| 峰值物理内存占用 (RSS)      | 188.4 MB (极致平稳直线)| 8,978.9 MB (~9.0 GB!)  |
| 物理内存节约比率            | 1.0× (基准)            | 47.6× 严重内存膨胀     |
| 百万级长期抓取稳定性        | 100% OOM 免疫安全      | 💥 内存无限泄漏必崩    |
+-------------------------------------------------------------------------------+
```

> [!TIP]
> **机器可复现数据源**：查阅全量 0.50.0 发版白皮书 [`docs/releases/0.50.0.md`](releases/0.50.0.md)，可在 [`bench/results/final/`](../bench/results/final/) 获取全部原始 JSON/CSV 评测明细。

---

## 0.50.0 全新特性

- 🧠 **动态自适应并发与超时引擎 (`pagesift::adaptive`)**：
  - **木桶短板原则 ($\min$ 原则)**：并发度严格受 CPU 算力、物理空闲内存（每连接预留 2MB）以及操作系统句柄限制（`ulimit -n`）的三者交集硬约束：
    $$\text{Concurrency} = \min\big(C_{cpu},\; M_{mem},\; FD_{safe}\big)$$
  - **全场景硬件规格自动识别**：覆盖 Micro（1C1GB）、Small（2C2GB~4C4GB）、Mid Desktop（8C16GB / i7-10700）、High Server（32C128GB / EPYC）与 Extreme Supernode（64C+ 128GB+ FD>=65k）。
  - **双模式运行开关**：支持 `--tuning-mode adaptive`（自适应满载吞吐）与 `--tuning-mode conservative`（严格压制 CPU < 20%，适合个人办公机后台静默运行）。
  - **跨国网络长 RTT 保活**：将基准连接超时精准校准至 3.5s、总超时 10.0s，彻底杜绝激进超时对欧美、亚太等跨国慢速站点的误杀。
- 🌐 **Firecrawl v1 & v2 双协议完全兼容**：官方 Python / Node SDK 零修改无缝接入，覆盖 `/v1/scrape`、`/v1/crawl`、`/v1/batch/scrape`、`/v1/map` 及 `/v2/*` 全量端点。
- ⚡ **Jina Reader 极简流式输出**：支持极简 `GET /r/{*url}` 与 `POST /r`，支持 `Accept: text/event-stream` SSE 实时流式直出 Markdown，为各类 LLM Agent 提供即插即用体验。
- 🛡️ **12 GiB 内存硬顶与全网 Chaos 对抗**：实测物理内存始终锁死在 **188.4 MB**，彻底终结 Spider 等竞品的 9GB+ 内存泄漏与死锁悬挂问题。
- 🦀 **100% 纯安全 Rust 与极致工程收敛**：全工作区严格保留 `#![forbid(unsafe_code)]`，仓库所有 Rust 源文件严格控制在 500 行以内。

---

## 快速开始

### 1. Docker / 本地自托管服务

启动独立容器（内置轻量无头浏览器渲染）：

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.50.0
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
pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --output ./data/
```

---

## 为什么选择 PageSift？

| 核心特性 | PageSift 0.50.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
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
