<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.51.0

**为 AI 智能体与 LLM 数据工程打造的高性能、严格内存有界抓取与内容提取引擎。**  
把任何网站、学术文档或历史档案转化为干净的 Markdown 与结构化数据——单个纯 Rust 二进制、原生多语言 SDK、完美兼容 Firecrawl v1/v2 与 Jina Reader 双协议，而内存占用仅为竞品零头。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![10万公网实测](https://img.shields.io/badge/10%E4%B8%87%E5%85%AC%E7%BD%91%E5%AE%9E%E6%B5%8B-47.6x%20%E5%86%85%E5%AD%98%E4%BC%98%E5%8A%BF-blue.svg)](#-旗舰决战128-gb-生产服务器-10-万公网顶级域名世纪决战)
[![128GB服务器防爆认证](https://img.shields.io/badge/128GB%E6%9C%8D%E5%8A%A1%E5%99%A8-OOM%20%E5%85%8D%E7%96%AB-success.svg)](#-10-大专项公开基准决战矩阵)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## ⚔️ 旗舰决战：128 GB 生产服务器 10 万公网顶级域名世纪决战

**硬件测试床**：**AMD EPYC 9B45 云生产超级服务器（32 物理核心，128 GB 内存，1Gbps+ 公网带宽）**  
**实测目标集**：全球权威 Tranco Top 100,000 活跃网站，在 768 异步工作池、3.5s 连接 / 10s 总超时、Blake3 全局去重与纯内存流式转换下同台实战：

```text
+-----------------------------------------------------------------------------------------+
| PageSift 0.50 vs Spider 2.53.9（32核 128GB 服务器真实公网 100,000 域名终极决战看板）    |
+-----------------------------------------------------------------------------------------+
| 对决指标维度                 | PageSift 0.50 (Rust)    | Spider 2.53.9 (Rust)   | 胜负判定
+------------------------------+-------------------------+------------------------+-------+
| 实际处理网站数               | 105,660 URLs            | 100,000 URLs           | 握手言和
| 真实 200 OK 响应页面         | 62,937 篇               | 72,209 篇 (含大量报错) | Spider
| 有效 Markdown 正文 (>100B)   | 60,659 篇富正文         | 59,803 篇              | PageSift (+856篇)
| 正文提取成功转化率           | 96.38% (极高保真)       | 82.82% (17.2% 空壳文档)| PageSift (质量碾压)
| 峰值物理内存占用 (RSS)       | 188.4 MB (超平平直横线) | 8,978.9 MB (~9.0 GB!)  | PageSift (低47.6倍)
| 百万级工业级持久作业能力     | 100% 具备 (零内存泄漏)  | 🔴 必定触发系统 OOM 斩杀| PageSift (工业级)
| 长时间运行死锁与泄漏         | 绝对零泄漏、零死锁      | 最终悬挂于 14 GB 泄漏死锁| PageSift (零泄漏)
+-----------------------------------------------------------------------------------------+
```

> [!IMPORTANT]
> **为什么面向工业级 AI 生产管线首选 PageSift：**
> - **物理内存低 47.6 倍**：经历 105,660 个全球公网站点的长时间持续冲击，PageSift 峰值常驻内存始终锁死在 **188.4 MB**。而 Spider 跑了仅仅 2.5 分钟就狂飙至 **8,978 MB（近 9 GB）**，最终在 14 GB 处因 Socket 悬挂彻底阻塞。
> - **正文清洗高保真**：Spider 生成了超过 12,400 篇空壳文档（提取失败率高达 17.2%）。PageSift 将 **96.38%** 的原始 HTML 转化为结构化 Markdown，**净交付有效正文总数（60,659 篇）全面超越 Spider（59,803 篇）**。
> - 完整机器可复现评测报告与复现脚本见 [`bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md)。

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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.51.0
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
