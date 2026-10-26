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

## 和同类工具比

按领先幅度分三档。0.80 的最终数字正在用发布二进制、和每个领域最强的竞品重跑；跑完之前表格留空，
不写没测过的数。每一行都会链接到方法、脚本和带日期的结果。

### 大幅领先

| 领域 | 指标（方向） | PageSift 0.80 | 最强竞品 | 证据 |
| :--- | :--- | :--- | :--- | :--- |
| _待最终对比跑完后填写_ | | | | |

### 领先

| 领域 | 指标（方向） | PageSift 0.80 | 最强竞品 | 证据 |
| :--- | :--- | :--- | :--- | :--- |
| _待最终对比跑完后填写_ | | | | |

### 落后但接近（正在努力开发中）

| 领域 | 指标（方向） | PageSift 0.80 | 最强竞品 | 证据 |
| :--- | :--- | :--- | :--- | :--- |
| _待最终对比跑完后填写_ | | | | |

在此之前，已经测得的最新结果（每项注明方向、版本和日期）见 [benchmarks.md](benchmarks.md)；
各领域的脚本和原始结果在 [benchmarks/](../benchmarks/)。

---

## 规模：单机 2 亿次请求（0.70）

- **192 vCPU、2 亿合成任务饱和测试**：200,000,000/200,000,000 完成，零失败，947,611 页/秒，211.1 秒，峰值 RSS 888.1 MiB。这是合成负载成绩，不能当作真实公网抓取速度。出处：[`0.70_200m_regression.md`](history/0.70_200m_regression.md)。
- **内存是输入**：引擎读取 cgroup v2 的 `memory.max` 来约束容量；在 768 MiB 限额下，实测稳定于 380 个连接、817,346 页/秒且没有 OOM。出处：[0.70.0 发行说明](releases/0.70.0.md)。

0.70.0 实测，之后的版本未重测。

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

更早的版本（0.73、0.72.8、0.71、0.70 等）见 [CHANGELOG](../CHANGELOG.md) 和[发行说明](releases/)。
首页撤下的段落（0.70 七领域对照、0.60 一亿次请求、0.50 十项矩阵、新旧版本混排的对比表等）连同出处原样保存在
[history/homepage-zh.md](history/homepage-zh.md)。

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

- **开源版本**：基于 [AGPL-3.0](../LICENSE) 许可协议开源。
- **商业授权**：如需闭源商业集成、免除 AGPL 开源义务或获取企业级技术支持，欢迎阅读 [商业授权指引](../COMMERCIAL.md) 或联系 `licensing@pagesift.dev`。
