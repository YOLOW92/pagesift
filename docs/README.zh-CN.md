<div align="center">

# pagesift 0.40.0

**为 AI 智能体与 LLM 数据工程打造的高性能、严格内存有界抓取与内容提取引擎。**  
把任何网站、学术文档或历史档案转化为干净的 Markdown 与结构化数据——单个纯 Rust 二进制、原生多语言 SDK、完美兼容 Firecrawl v1/v2 与 Jina Reader 双协议，而内存占用仅为竞品零头。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![12GiB 内存防爆认证](https://img.shields.io/badge/12GiB%20%E5%86%85%E5%AD%98%E9%98%B2%E5%BE%A1-%E5%AE%9E%E6%B5%8B%20OOM%20%E5%85%8D%E7%96%AB-success.svg)](#-8-大专项公开基准决战)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## 🏆 8 大专项公开基准决战（数据全面对比）

全部评测均在物理机 **Intel Core i7-10700**（8 物理核心，16 逻辑超线程，15.5 GB 物理内存）下同台执行，系统常驻 **12 GiB 内存硬防御看门狗**（`MemoryMax=12288M`）。完整可验证原始 CSV 数据集归档于 [`bench/results/final/`](../bench/results/final/)。

| 专项评测领域 | PageSift 0.40.0 实测表现 | 核心竞品基准表现 | 决战性能优势 / 提速倍率 | 峰值物理内存（PageSift vs 竞品） |
| :--- | :--- | :--- | :--- | :---: |
| **01. 正文提取与并发** | **3,525 页/秒** (0.57s) | `rs-trafilatura`: 241 页/秒 (8.32s)<br>`trafilatura` (Py): 191 页/秒 (10.51s) | **14.6× ~ 18.4× 提速** | **137.8 MB** vs 298.3 MB |
| **02. 5万页爬取与 12G 防爆** | **2,727 页/秒** (17.76s)<br>峰值仅 **447.5 MB** | `Spider 2.53.9`: 内存飙升至 11.7 GiB<br>→ **触发 12G 守护直接 OOM 斩杀** | **绝对零内存泄漏**<br>（内存仅为竞品 1/26） | **447.5 MB** vs 11,724.8 MB (💥 OOM 崩溃) |
| **03. PDF 解析与学术公式** | 通过率 **71.0%**，LaTeX 公式 **44.3%**<br>**3.4 ms/页** | `pymupdf4llm`: 通过率 64.4%，LaTeX 0.0%<br>耗时 320.0 ms/页 | **94× 提速**<br>原生 TeX 公式解析独占 | **42.0 MB** vs 280.0 MB |
| **04. DOM 自愈与深层变异** | 找回率 **93.8% (469/500)**，**0.0% 误报**<br>耗时仅 **24.6 µs** | `Scrapling`: 找回率 46.0%，**误报率 51.2%**<br>耗时 635.3 µs | **25.8× 提速**，零诱饵误报<br>200层深 DOM 栈安全 (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. RAG 分块与 BM25** | R@1000 = **0.895** (切分 19.7 ms)<br>BM25 过滤：**1.35 ms/题** | `semchunk`: 切分 113.4 ms (慢 5.8×)<br>`Crawl4AI`: 过滤 804.8 ms/题 (**慢 595×**) | **5.8× 分块提速**<br>**595× BM25 检索过滤提速** | **45.2 MB** vs 340.0 MB |
| **06. 多语言历史编码探测** | 准确率 **74.4% (2336/3138)**，Unicode **99.7%**<br>**0.45 ms/文件** | `chardet 7.7`: 准确率 68.9% (3.05 ms)<br>`charset-normalizer`: 准确率 46.5% (1.05 ms) | **准确率更高 (+5.5%)**<br>**6.8× 提速** | **18.5 MB** vs 42.0 MB |
| **07. 深层结构化数据** | 标准微数据 **80.2 µs/页**<br>40 层极深嵌套 **363 µs** | `extruct 0.18`: 3,508 µs/页 (慢 43.8×)<br>40 层极深嵌套 15,921 µs | **43.8× ~ 43.9× 提速**<br>线性复杂度零堆栈溢出 | **24.0 MB** vs 85.0 MB |
| **08. 隐式 JS 端点发现** | **13/13 (100.0%)** 满分召回<br>**0 次非导航违规访问**，**0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 次非导航违规访问 (30.8% 误访问)，48.5 ms | **100% 召回**，零诱饵泄漏<br>零无头浏览器沙箱开销 | **14.2 MB** vs 128.0 MB |

> [!TIP]
> **机器可复现数据源**：查阅全量 8 项测试白皮书 [`docs/releases/0.40.0.md`](releases/0.40.0.md)，可在 [`bench/results/final/`](../bench/results/final/) 获取全部原始 CSV 评测明细。

---

## 0.40.0 全新特性

- 🌐 **Firecrawl v1 & v2 双协议完全兼容**：官方 Python / Node SDK 零修改无缝接入，覆盖 `/v1/scrape`、`/v1/crawl`、`/v1/batch/scrape`、`/v1/map` 及 `/v2/*` 全量端点。
- ⚡ **Jina Reader 极简流式输出**：支持极简 `GET /r/{*url}` 与 `POST /r`，支持 `Accept: text/event-stream` SSE 实时流式直出 Markdown，为各类 LLM Agent 提供即插即用体验。
- 🛡️ **12 GiB 内存硬顶与 10 万 Chaos 对抗**：在 50,000 页实测中，竞品 Spider 因无界内存泄露达 11.7 GiB 被系统 OOM 斩杀，PageSift 物理内存始终锁死在 **447.5 MB**（仅为竞品 1/26），吞吐高达 **2,727 页/秒**；从容应对 100,000 URLs 混沌集群（150 层病态 DOM、3,000 次 503 故障与无限陷阱）。
- 🦀 **100% 纯安全 Rust 与极致工程收敛**：`pagesift-api` 严格保留 `#![forbid(unsafe_code)]`，仓库所有 Rust 源文件严格控制在 500 行以内。

---

## 快速开始

### 1. Docker / 本地自托管服务

启动独立容器（内置轻量无头浏览器渲染）：

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.40.0
```

#### Firecrawl SDK 零代码切换
只需修改 API 地址，现有应用无需变动任何业务代码：

```python
from firecrawl import FirecrawlApp

# 直接指向本地 PageSift 实例
app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### Jina Reader 极简直出
面向任意 LLM Agent 或命令行快速获取 Markdown：

```bash
# 单 URL 纯净 Markdown 直出
curl -s http://localhost:3002/r/https://en.wikipedia.org/wiki/Rust_(programming_language)

# SSE 实时流式推送
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Python 原生包

```bash
pip install pagesift
```

```python
import pagesift

# 单页微秒级极速提取 (< 2ms)
page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

# 内存有界的大规模并发抓取
results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Node.js / TypeScript 原生包

```bash
npm install @pagesift/node
# 或者
npm install pagesift
```

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

// 抓取并转化为大模型友好的纯净 Markdown
const page = await scrape('https://example.com', {
  formats: ['markdown', 'links']
})
console.log(page.markdown)

// 并发流式异步迭代
const stream = scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })
for await (const page of stream) {
  console.log(`抓取完成: ${page.url} 耗时 ${page.elapsedMs}ms`)
}
```

---

### 4. 命令行工具 (CLI)

```bash
# 通过 cargo 安装独立命令行工具
cargo install pagesift --locked

# 抓取单页面
pagesift scrape "https://news.ycombinator.com" --format markdown

# 高并发礼貌爬取并导出数据
pagesift crawl "https://docs.rs" --limit 5000 --concurrency 16 --output ./data/
```

---

## 竞品全方位横向对比

| 功能特性 | PageSift 0.40.0 | Firecrawl (自托管) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **底层核心架构** | 纯安全 Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **协议兼容生态** | **Firecrawl v1+v2 + Jina + MCP** | 仅 Firecrawl 原生协议 | 自定义 Python 接口 | 自定义 REST 接口 |
| **正文提取吞吐** | **3,525 页/秒** | 约 150 页/秒 | 约 90 页/秒 | 约 1,200 页/秒 |
| **5万页爬取峰值内存** | **447.5 MB (严格有界)** | > 6 GB (Redis 堆积) | > 8 GB (浏览器泄露) | 💥 **OOM 崩溃 (11.7 GB)** |
| **DOM 元素智能自愈** | **93.8% (0.0% 零误报)** | ❌ 无 | ❌ 无 | ❌ 无 |
| **学术 PDF 与公式** | **71% 通过 / 44.3% LaTeX** | 外部库转换纯文本 | 仅提取无序纯文本 | 仅提取无序纯文本 |
| **RAG 向量分块与 BM25** | **19.7 ms / 1.35 ms BM25** | ❌ 无 | Python 分块 (804 ms) | ❌ 无 |
| **爬虫道德与合规** | 守规矩、按主机限速、诚实 UA | 代理池频繁对抗 | 无头隐匿对抗 | 无头隐匿对抗 |

---

## 核心架构设计

- **分段有界调度器（Segmented Bounded Frontier）**：抓取队列绝不随抓取深度无限膨胀，通过自适应背压与紧凑布隆过滤，即使抓取十万级 URL 也彻底隔绝爆内存风险。
- **自适应智能渲染（Selective Rendering Pipeline）**：自动检测静态与动态页面；仅当动态 JavaScript 执行能带来实际内容收益时，才按需调度 Chrome 无头渲染。
- **全方位 SSRF 与沙箱防护**：严格过滤私网内网 IP，防范 DNS 重绑定攻击，永不禁用浏览器安全沙箱。
- **企业级服务可靠性**：内置任务持久化、Webhook 顺序可靠回调、按 API Key 令牌桶限流与 Prometheus 格式监控指标。

---

## 开源协议与商业授权

- **开源版本**：基于 [AGPL-3.0](LICENSE) 协议发布。
- **商业闭源授权**：若需在专有软件中静态集成或私有商业化闭源部署，欢迎参阅 [商业授权说明](../COMMERCIAL.md) 或联系 `licensing@pagesift.dev`。
