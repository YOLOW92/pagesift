<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/docs/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.5

**把网页、PDF 和 Office 文件转成供 LLM 和 AI 智能体使用的干净 Markdown。**  
一个自托管的 Rust 二进制：CLI、兼容 Firecrawl 的 HTTP API、Jina Reader 端点、MCP 服务器、Python 和 Node.js 包。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![PageSift MCP server – quality and maintenance score on Glama](https://glama.ai/mcp/servers/YOLOW92/pagesift/badges/score.svg)](https://glama.ai/mcp/servers/YOLOW92/pagesift)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

<!-- readme-template: v1 -->
<!-- section: overview -->
## 它是什么

pagesift 抓取一个 URL（只在页面需要时才渲染 JavaScript）或读取本地文件，保留正文，
返回 Markdown 和元数据。它能抓单个页面、批量页面和整站，
可以作为命令、库、HTTP 服务或 MCP 服务器运行。

可以替代或配合使用：**Firecrawl**（同样的 v1/v2 API，自托管）、**Jina Reader**（`/r/<url>`）、
**Crawl4AI**、**trafilatura** / **Readability**、**pymupdf4llm** / **pypdf**、**Kreuzberg**。

<!-- section: facts -->
## 速览

- **输入：** URL（HTTP，必要时用无头浏览器）、HTML、PDF、Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）、EPUB、CSV
- **输出：** Markdown、HTML、原始 HTML、链接、图片、元数据（标题、发布日期……）、JSON；结果不完整时给出警告
- **接口：** CLI · Firecrawl v1/v2 HTTP API · Jina Reader `/r/` · MCP（stdio 或 HTTP）· Python · Node.js / TypeScript · Rust crate
- **平台：** Linux x86_64 / aarch64（glibc ≥ 2.28 和 musl）、macOS x86_64 / arm64、Windows x86_64；Docker 镜像 `ghcr.io/yolow92/pagesift`
- **爬取：** 默认遵守 robots.txt 并按主机限速，支持站点地图、按查询的聚焦爬取，内存有界
- **内存：** 0.81 各项基准中每个 worker 峰值 14–380 MB（16 路处理 HTML、PDF 时最低）；容量按 cgroup 内存上限自动确定（768 MiB 下无 OOM）
- **不适用：** 扫描版 PDF 或图片的 OCR；pagesift 只读取文本层（见下文“什么时候用”）
- **许可证：** AGPL-3.0，或[商业许可](COMMERCIAL.md)
- **最新版本：** 0.81.5 · [发行说明](releases/0.81.5.md)

<!-- section: install -->
## 安装与使用

| | 安装 | 第一次调用 |
|---|---|---|
| CLI | `cargo install pagesift --locked` | `pagesift scrape https://example.com` |
| Python | `pip install pagesift` | `pagesift.scrape("https://example.com").markdown` |
| Node.js | `npm install @pagesift/node` | `(await scrape("https://example.com")).markdown` |
| 服务器 | `docker run -p 3002:3002 ghcr.io/yolow92/pagesift:0.81.5` | `curl localhost:3002/r/https://example.com` |
| MCP | — | `claude mcp add pagesift -- pagesift mcp` |

**CLI**

```bash
pagesift scrape "https://news.ycombinator.com"              # Markdown 输出到 stdout
pagesift scrape report.pdf                                  # 本地 PDF、Office、EPUB、CSV 或 HTML 文件
pagesift crawl "https://docs.rs" --limit 500 --out ./data/  # 每页一个 Markdown 文件
pagesift crawl "https://www.example.edu" --query "graduate admission" --adaptive
pagesift map "https://docs.rs"                              # 列出站点的 URL
pagesift doctor                                             # 检查浏览器和运行环境
```

**Python**

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

**Node.js / TypeScript**

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

const page = await scrape('https://example.com', { formats: ['markdown', 'links'] })
console.log(page.markdown)

for await (const p of scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })) {
  console.log(p.url, p.elapsedMs)
}
```

**HTTP 服务器（兼容 Firecrawl 和 Jina Reader）**

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.5
```

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")  # 现有 Firecrawl 代码，只换 URL
print(app.scrape_url("https://example.com", params={"formats": ["markdown"]})["markdown"])
```

```bash
curl -s http://localhost:3002/r/https://example.com -H 'Authorization: Bearer sk-test'   # Jina Reader 风格
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'                            # 上传文件
```

端点：`/v1` 和 `/v2` 的 `scrape`、`batch/scrape`、`crawl`、`map`；`/v2/parse`（文件上传，最大 50 MB，
[详情](parse-api.md)）；`/r/<url>`（纯 Markdown 或 `text/event-stream`）。

**供 AI 智能体使用的 MCP 服务器**

```bash
pagesift mcp                                    # stdio，供单个智能体使用
pagesift mcp http --port 3003                   # 一个共享的本地服务（仅 127.0.0.1）
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

工具：`pagesift_scrape`、`pagesift_batch_scrape`、`pagesift_crawl`、`pagesift_crawl_status`、`pagesift_map`。
共享浏览器和缓存、审计日志和问题上报工具：[共享 MCP 服务](mcp-service.md)。

<!-- section: choose -->
## 什么时候用，什么时候不用

**适合**

- 把网页和文档喂给 LLM、RAG 索引或智能体，Markdown 保留标题、列表、表格和链接
- 在自己的机器上自托管 Firecrawl 或 Jina Reader 式的服务，不按页付费
- 混合输入：同一个调用处理 HTML、PDF、Office 文件和电子书
- 小机器和容器：内存有界，按 cgroup 限额确定大小

**这些情况选别的**

- **扫描版 PDF 或图片需要 OCR。** pagesift 只读文本层；在 134 页扫描件上完成率 0.112，GPU 上的 Docling 为 1.000。请用 Docling、olmOCR 或其他 OCR 流程。
- **你要最快的爬取速度，不需要礼貌限速。** pagesift 对同一主机的请求之间会等待、遵守 robots.txt 并重试失败；spider 的纯爬取耗时快得多（40 站 × 60 页上 1.4 s 对 16.6 s）。
- **AGPL 对你不合适**，商业许可也不可行：请用宽松许可证的工具。
- **你现在就需要托管的云服务。** pagesift 是自托管的；托管 API 还没开放。

<!-- section: performance -->
## 性能

<!-- perf-glance -->
↑ 越高越好 · ↓ 越低越好。每个领域取最强的一项；方法和出处见完整报告。

| 任务 | 指标 | PageSift | 最好的其他工具 | 差距 |
|---|---|---:|---|---:|
| PDF → Markdown — olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 ↓ | **2.2** | pypdf 50.0 | **×22.32** |
| HTML → Markdown — 600 篇生成文档，单页 | 单页耗时 ↓ | **0.24** | Turndown 1.9 | **×8.24** |
| 网页正文 — Firecrawl 597 页，单线程 | 单页中位耗时 ↓ | **1.0** | crw 7.4 | **×7.23** |
| 发布日期 — htmldate 1,000 页，日期搜索 | 单页耗时 ↓ | **0.31** | htmldate (fast) 2.0 | **×6.45** |
| Office / 电子书 — B4 81 份，单进程 | 吞吐 ↑ | **94.1** | Kreuzberg 19.5 | **×4.82** |
| RAG 分块 — 5 份语料，1.4 MB | 总耗时 ↓ | **11** | chonkie recursive 35 | **×3.24** |
| RAG 查询过滤 — 472 题，默认配置 | 答案召回 ↑ | **0.929** | Crawl4AI BM25 0.673 | **+0.256** |
| 真实站点抓取 — polite，40 站 × 60 页，覆盖 | 覆盖率 ↑ | **0.740** | Crawl4AI (HTTP) 0.529 | **+0.211** |

**仍落后：** A2 回放 269 页，1 路 2.4 pages/s vs spider 156.6 pages/s；fast，40 站 × 60 页，耗时 16.6 seconds vs spider 1.4 seconds；B5 134 页，完成率 0.112 vs Docling (GPU) 1.000。

*A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。*

[完整报告](../benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

规模（0.70，合成的本地源站，不是真实互联网速度）：192 vCPU 上 200,000,000 次请求零失败，
947,611 页/秒，峰值 RSS 888 MiB；在 768 MiB 的 cgroup 内仍维持 817,346 页/秒，没有 OOM。
出处：[0.70 测试](history/0.70_200m_regression.md)、[全部基准](benchmarks.md)。

<!-- section: features -->
## 功能

- **提取：** 正文识别；Markdown 能完整往返标题、列表、表格、代码和数学公式；发布日期；`includeTags` / `excludeTags` CSS 筛选；结果不完整时给出 `partial_content`、`pages_missing`、`output_truncated` 等警告，而不是悄悄返回残缺结果
- **文档：** 自研的 Rust 读取器，支持 PDF（结构树、书签、页眉页脚、没有 ToUnicode 的 CJK 字体、损坏文件）、Word、Excel、PowerPoint、EPUB 和 CSV
- **渲染：** `engine: auto` 先用 HTTP 抓取，只在能多拿到内容时才打开无头浏览器；Docker 镜像自带一个
- **爬取：** 聚焦爬取（`--query … --adaptive`），页面不再带来相关内容时停止；附件（链接的 PDF 和 Office 文件）；每页的爬取轨迹；站点地图；重复和陷阱 URL 处理；按主机的令牌桶，遇到 429/503 退避
- **运维：** 有界队列，容量按 CPU 和 cgroup 内存确定；服务器端有任务持久化、webhook、按 key 限额和指标；每次抓取都记审计日志
- **安全：** 默认防护私有网络和 DNS 重绑定（用 `--allow-cidr` 放行），浏览器沙箱

架构：[architecture.md](architecture.md)。

<!-- section: license -->
## 许可证

[AGPL-3.0](../LICENSE)。如需专有链接或不承担 AGPL 义务的私有部署，请见
[商业许可](COMMERCIAL.md) 或写信到 `licensing@pagesift.dev`。

<!-- section: changelog -->
## 0.81.5 更新

Linux glibc 版本（Node 绑定和命令行二进制）改为针对 glibc 2.28 编译，可在更老的发行版上加载；
0.81.4 及以前需要 glibc 2.34 到 2.35 的符号。API 没有变化。
[发行说明](releases/0.81.5.md) · [全部版本](releases/) · [CHANGELOG](../CHANGELOG.md)

---

## 社区

问题或反馈：[Discord](https://discord.gg/esA3wMZVvA) · [Issues](https://github.com/YOLOW92/pagesift/issues) · [Discussions](https://github.com/YOLOW92/pagesift/discussions)

## 引用

```bibtex
@software{pagesift2026,
  title  = {PageSift: Rust Web and Document Extraction Engine for AI Pipelines},
  author = {YOLOW92},
  year   = {2026},
  url    = {https://github.com/YOLOW92/pagesift},
  note   = {Version 0.81.5}
}
```

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
