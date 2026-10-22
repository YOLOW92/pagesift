<div align="center">

# pagesift

**为 LLM 和 AI 智能体打造的快速、守规矩的网页抓取工具。**
把任何网站变成干净的 Markdown 和结构化数据——一个 Rust 二进制文件，兼容 Firecrawl 的 API，内存占用只是零头。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/pagesift.svg)](https://www.npmjs.com/package/pagesift)

[English](../README.md) · **简体中文** · [日本語](README.ja.md) · [Español](README.es.md)

</div>

---

## 0.20.0

0.20.0 已加入原生 Rust 增量采集、独立 HTTP 会话、可验证归档，以及七类有明确边界的新核心。新增命令主要通过 Rust API / CLI 提供；17 类能力仍为部分实现，不宣称六项目全功能领先。

[Release / migration](releases/0.20.0.md) · [Documentation](README.md) · [Architecture](architecture.md) · [Evidence](../bench/six/evidence/README.md)

![pagesift 0.20.0](assets/overview.svg)

pagesift 是一个可自托管的抓取与爬取引擎，面向 AI 流水线、RAG 和智能体。它负责获取页面，只在必要时才渲染 JavaScript，提取正文，并通过兼容 [Firecrawl v2](https://docs.firecrawl.dev) 的 HTTP API、MCP 服务器、CLI 以及原生 Python 和 Node 包返回 Markdown、HTML、链接和元数据。

它为以服务形式运行而设计：内存有上限、支持背压、任务持久化、webhook、按密钥限流以及指标监控。它也守规矩——遵守 robots.txt，按主机限速，使用诚实的 User-Agent，不做任何反爬绕过。

## 为什么选择 pagesift

| | |
|---|---|
| ⚡ **快** | 正文提取每页仅需 **0.85 ms**（p50，单线程）：比 crw 快 16 倍，比 Crawl4AI 快 80 倍。不限速的 HTTP 爬取在单台机器上可达 **3,800+ 页/秒**。 |
| 🪶 **轻** | 爬取 200 个站点 × 100 页只需 **约 300 MB**，而 Spider 需要 4.1 GB。在一个生产服务中替换掉 Crawl4AI 后，峰值内存从 1.4 GB 降到 **225 MB**。 |
| 🎯 **准** | 在 Firecrawl 的标注数据集上取得最佳正文得分：**0.716**，对比 0.711（crw）和 0.670（Crawl4AI），样板内容泄漏只有一半。发布日期的准确率与专门的日期库 htmldate 持平（在它自己的标注集上 F1 **0.948**），每页只要 **0.06 ms**，htmldate 要 4 ms。 |
| 🧠 **智能渲染** | 只渲染真正需要的页面，并选用能胜任的最轻量引擎：Lightpanda → moli → Chrome。会按站点学习浏览器何时毫无增益。 |
| 🔌 **即插即用** | Firecrawl v2 API（`/v2/scrape`、`/crawl`、`/map`、`/search`、`/extract`、`/batch/scrape`），现有 Firecrawl SDK 无需改动即可使用。另提供面向智能体的 MCP 服务器。 |
| 🛡️ **默认安全** | 对每个请求、重定向、浏览器子请求和 webhook 都做 SSRF 防护；检查 DNS 重绑定；从不关闭浏览器沙箱。 |

详细、可复现的数据：**[docs/benchmarks.md](benchmarks.md)**。

## 快速开始

### Docker

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=change-me ghcr.io/yolow92/pagesift:0.20.0

curl -s http://localhost:3002/v2/scrape \
  -H 'Authorization: Bearer change-me' -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com", "formats": ["markdown", "links"]}'
```

镜像内置 Lightpanda，用于渲染 JavaScript。

### 命令行

```bash
# from a release binary (Linux, macOS) or with Cargo
cargo install pagesift

pagesift scrape https://example.com                 # Markdown to stdout
pagesift crawl https://example.com --out ./site     # one Markdown file per page
pagesift map https://example.com                    # list a site's URLs
pagesift serve --port 3002                          # the HTTP API
pagesift doctor                                     # check browsers, network, config
```

### Python

```bash
pip install pagesift
```

```python
import pagesift

doc = pagesift.scrape("https://example.com")
print(doc.markdown)

# many pages at once, on every core
for doc in pagesift.scrape_iter(["https://example.com/a", "https://example.com/b"]):
    print(doc.metadata["title"])
```

### Node.js

```bash
npm install pagesift
```

```js
const { Scraper } = require('pagesift')

const scraper = new Scraper()
const page = await scraper.scrape('https://example.com')
console.log(page.markdown)
```

### MCP

```bash
pagesift mcp            # stdio; tools: pagesift_scrape, pagesift_batch_scrape, pagesift_crawl, pagesift_crawl_status, pagesift_map
```

## 功能

**抓取与爬取**
- 输出格式：`markdown`、`html`（已清理）、`rawHtml`、`links`、`images`；配合任意兼容 OpenAI 的 LLM 可输出 `summary` 和 `json`（按 schema 提取）；`changeTracking`（与上次访问做差异对比，需要启用缓存）。
- 爬取支持包含/排除路径、深度与数量限制、sitemap 和 robots.txt；**查询聚焦爬取**（`query`、`adaptive`）优先访问最相关的页面，并在新页面不再带来帮助时停止。
- 忠实的 Markdown：表格、多层列表、带语言标注的代码块、硬换行和公式（KaTeX、MathJax、MathML、维基百科公式都转成 `$…$` 形式的 TeX）；正文里的 Markdown 特殊字符只在会被误解析成格式时才转义。随机往返测试得分 1.000，第二名是 0.937。
- **在真实网站上爬取不浪费额度：** 去掉跟踪参数和会话 ID，跳过二进制文件和无限路径，自动学会哪些 URL 是同一页面的别名，把日历、翻页这类陷阱排到最后，临时失败稍后重试，站点返回 429 时自动放慢。在模拟的杂乱网络上，同样的页数上限内找到 71% 的内容页，第二名是 49%（见基准测试）。
- `queryFilter` 只保留与查询相关的内容：页面先在各小节内切成约 100 token 的单位，带着标题路径用 BM25 打分，保留得分不低于最高分一半的单位，按原文顺序输出并补上标题。在一个带标注的 RAG 评测集上，平均用约 600 词保留了 93% 的答案原文；Crawl4AI 的 BM25 过滤器用 1.35 万词只保留了 67%，速度也慢 70 倍。另有 `maxTokens`、`topK` 和绝对阈值 `threshold` 可选。
- `chunking` 把 Markdown 切成可直接用于 RAG 的块，每块带标题路径和位置：`recursive`（限定大小，在最有意义的边界切开，代码块和标题与其内容保持在一起；移植自 semchunk，检索质量相同，速度快 3 倍多）、按标题、段落、句子、token 窗口。
- PDF 文本提取；元数据（标题、描述、语言、作者、发布时间、Open Graph）。发布日期按 htmldate 的方式检索（meta 标签、JSON-LD、日期元素、正文模式），支持约二十种语言的月份名，并保留页面自己的时区。
- 条件请求（`ifNoneMatch`、`ifModifiedSince`）、内容缓存（`maxAge`）、用于变更检测的内容哈希。

**渲染**
- 自动选择引擎并回退（Lightpanda、moli、Chrome/Chromium/chrome-headless-shell），也可通过 CDP 连接远程浏览器。
- 自适应浏览器池，根据实测吞吐量、CPU 和可用内存确定规模；同源 iframe 会被内联。
- 默认屏蔽广告、跟踪器、字体和媒体资源。

**以服务形式运行**
- 任务持久化并可在重启后恢复，webhook 支持重试和签名，按 API 密钥设置速率、并发和每日配额。
- Prometheus 指标（`/metrics`）、JSON 日志、优雅停机、健康检查。
- 按请求指定的命名出口代理、面向内部服务的主机白名单、单次请求的总截止时间、清晰的错误码（`DNS_FAILURE`、`NETWORK_ERROR`、`BUSY`、`ACCESS_BARRIER`、…）。

## 对比

| | pagesift | Crawl4AI | Firecrawl | crw | Spider |
|---|---|---|---|---|---|
| 语言 | Rust | Python | TypeScript | Rust | Rust |
| 可自托管的单一二进制 | ✅ | ❌ | ❌ | ✅ | ✅ |
| 兼容 Firecrawl 的 API | ✅ | ❌ | ✅ | ✅ | ❌ |
| 正文提取得分 ↑ | **0.716** | 0.670 | — | 0.711 | — |
| Markdown 保真度（随机往返测试）↑ | **1.000** | 0.784 | — | — | — |
| RAG 查询过滤：保留的答案 / 返回词数 | **93% / 1,150** | 67% / 13,500 | — | — | — |
| 每页提取耗时 p50 ↓ | **0.85 ms** | 50.2 ms | — | 8.93 ms | — |
| HTTP 爬取，200 × 100 页 ↑ | **3,890 p/s** | 17 p/s | — | 48 p/s | 2,090 p/s |
| 该次爬取的内存 ↓ | **218 MB** | 946 MB | — | 447 MB | 4.1 GB |
| 杂乱站点爬取：找到的内容页 ↑ | **71.0%** | 49.1% | — | 35.4% | 34.7% |
| 杂乱站点爬取：浪费的请求 ↓ | **13%** | 44% | — | 45% | 45% |
| 轻量 JS 引擎（Lightpanda/moli） | ✅ 两者皆有，自动回退 | ❌ | ❌ | Lightpanda | — |
| MCP 服务器 | ✅ | ✅ | ✅ | ✅ | — |
| 原生 Python / Node 包 | ✅ / ✅ | ✅ / ❌ | HTTP SDK | — | ✅ / ✅ |
| 许可证 | AGPL-3.0 + 商业许可 | Apache-0.20.0 | AGPL-3.0 | AGPL-3.0 | MIT |

“—”表示未检测。未对 Firecrawl 的自托管服务做基准测试。测试方法和完整表格见 [docs/benchmarks.md](benchmarks.md)。

## 配置

使用默认配置即可正常工作。部署服务器时，复制 [`pagesift.example.toml`](../pagesift.example.toml)，然后传入 `--config` 或设置 `PAGESIFT_CONFIG`。它涵盖 API 密钥与限额、webhook、User-Agent、速率限制、浏览器引擎、缓存、LLM 与搜索后端、出口代理以及允许访问的内部主机。

## 负责任的爬取

pagesift 遵守 robots.txt（包括 `Crawl-delay`），对每个主机限速，遇到 429 和 5xx 时退避，并使用诚实的 User-Agent 表明身份。当站点出现验证挑战或拒绝访问页面时，pagesift 会如实报告（`ACCESS_BARRIER`），而不是设法绕过。我们不接受添加隐身、指纹伪装、CAPTCHA 破解或代理轮换的 Pull Request。

## 许可证

pagesift 采用双许可证：

- **[AGPL-3.0](../LICENSE)** 用于开源用途。可以自托管、修改、商用；如果你通过网络向他人提供修改后的版本，需要公开你的修改。
- **[商业许可证](../COMMERCIAL.md)** 用于在不承担 AGPL 义务的情况下使用 pagesift——闭源产品、专有托管服务——并可选购技术支持。

## 参与贡献

欢迎提交 Issue 和 Pull Request；请先阅读 [CONTRIBUTING.md](../CONTRIBUTING.md)。贡献者只需签署一次 [CLA](../CLA.md)。安全问题报告：[SECURITY.md](../SECURITY.md)。
