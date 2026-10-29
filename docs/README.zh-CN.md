<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.3

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

<!-- perf-glance -->
## 性能一览 (0.81)

PageSift 0.81 于 2026-10-29 新测；竞品为 2026-10-21/22/27/28 的原始数据，未重跑。每行一个工作负载，PageSift 取该负载的最佳配置；内存单列，不单独进榜。


### 大幅领先

| 领域 | 工作负载 | 指标 | PageSift 0.81 | 最快对手 | 对手 | 差距 | 峰值内存 MB（PageSift / 对手） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 页，单页（单线程） | 单页耗时 | **2.2** | pypdf | 50.0 | **×22.32** | — |
| HTML → Markdown | 600 篇生成文档，单页 | 单页耗时 | **0.24** | Turndown | 1.9 | **×8.24** | — |
| 网页正文 | Firecrawl 597 页，单线程 | 单页中位耗时 | **1.0** | crw | 7.4 | **×7.23** | — |
| 发布日期 | htmldate 1,000 页，日期搜索 | 单页耗时 | **0.31** | htmldate (fast) | 2.0 | **×6.45** | — |
| Office / 电子书 | B4 81 份，单进程 | 吞吐 | **94.1** | Kreuzberg | 19.5 | **×4.82** | 38 / 231 |
| RAG 分块 | 5 份语料，1.4 MB | 总耗时 | **11** | chonkie recursive | 35 | **×3.24** | — |
| PDF → Markdown | B1 144 页，1 路 | 吞吐 | **258.5** | pdf_oxide | 100.6 | **×2.57** | 163 / 29 |
| RAG 查询过滤 | 472 题，默认配置 | 答案召回 | **0.929** | Crawl4AI BM25 | 0.673 | **+0.256** | — |
| 真实站点抓取 | polite，40 站 × 60 页，覆盖 | 覆盖率 | **0.740** | Crawl4AI (HTTP) | 0.529 | **+0.211** | — |
| 真实站点抓取 | fast，40 站 × 60 页，覆盖 | 覆盖率 | **0.685** | Crawl4AI (HTTP) | 0.499 | **+0.186** | — |
| Office / 电子书 | B4 留出集 39 份，结构保留率 | 结构保留率 | **0.988** | Kreuzberg | 0.816 | **+0.172** | — |
| PDF → Markdown | olmOCR-bench 1,269 页 | 官方总分 | **0.628** | pymupdf4llm | 0.484 | **+0.144** | — |


### 领先

| 领域 | 工作负载 | 指标 | PageSift 0.81 | 最快对手 | 对手 | 差距 | 峰值内存 MB（PageSift / 对手） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | B1 144 页，16 路 | 吞吐 | **580.6** | pdf_oxide | 501.7 | **×1.16** | 14 / 11 |
| 网页正文 | A2 随机 269 页，16 路 | 吞吐 | **630.0** | Resiliparse | 559.3 | **×1.13** | 17 / 54 |
| HTML → Markdown | 600 篇生成文档，保真度 | 平均 F1 | **1.000** | markdownify | 0.936 | **+0.064** | — |
| 网页正文 | WCXB 测试集 150 页 | F1 | **0.866** | Readability + Turndown | 0.848 | **+0.018** | — |


*A2 的 269 页于 2026-10-29 重新抓取，绝对耗时不能和 10-28 的 A2 直接比较。*


[完整报告](../benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

---

## 和同类工具比

**仍落后：** A2 回放 269 页，1 路 2.4 pages/s vs spider 156.6 pages/s；fast，40 站 × 60 页，耗时 16.6 seconds vs spider 1.4 seconds；B5 134 页，完成率 0.112 vs Docling (GPU) 1.000。

[完整报告](../benchmarks/history/0.81/results/20261029-102426/report.md).

---

## 规模：单机 2 亿次请求（0.70）

- **192 vCPU、2 亿合成任务饱和测试**：200,000,000/200,000,000 完成，零失败，947,611 页/秒，211.1 秒，峰值 RSS 888.1 MiB。这是合成负载成绩，不能当作真实公网抓取速度。出处：[`0.70_200m_regression.md`](history/0.70_200m_regression.md)。
- **内存是输入**：引擎读取 cgroup v2 的 `memory.max` 来约束容量；在 768 MiB 限额下，实测稳定于 380 个连接、817,346 页/秒且没有 OOM。出处：[0.70.0 发行说明](releases/0.70.0.md)。

0.70.0 实测，之后的版本未重测。

---

## 0.81.3 更新

**标签筛选与 Jina 路由鉴权。** Firecrawl 的 `includeTags` / `excludeTags` 正式生效：按 CSS 选择器（标签名、`.class`、`#id` 等）在提取正文前保留或删掉元素，`<head>` 元数据照常提取，`rawHtml` 不变；选择器写错在抓取前就报错。Jina Reader 路由（`/r/`）在配置了 API key 时也要带 key，并计入每分钟限流和页数配额。详见 [0.81.3 发行说明](releases/0.81.3.md)。

## 0.81.2 更新

**自适应升级零正文损失。** HTTP 正文为 0 的裸客户端跳转页，现在目标页主框架一出现文字就抓取当前 DOM，不再等 `DOMContentLoaded` 或网络静默；等待上限按本页 HTTP 耗时和调用方总时限缩放。A2 269 页回放：0.81.0 的 110.7 s / 21.9 s（1/16 worker）降到 88.0 s / 13.5 s，逐页对比 **没有任何页面正文减少**，总量多 174 字符。详见 [0.81.2 发行说明](releases/0.81.2.md)。

## 0.81.1 更新

**自适应浏览器升级。** `engine: auto` 在正文不再增加时停止浏览器升级，用导航、稳定等待的在线分布代替固定秒数。A2 269 页回放：16 worker 从 28.4 s 降到 10.5 s（9.5 → 25.7 页/s），1 worker 从 119.2 s 降到 77.3 s；全部页面正文只少 301 字符。HTTP 正文为 0 的页面仍然保留完整渲染时间。详见 [0.81.1 发行说明](releases/0.81.1.md)。

## 0.81.0 更新

**相关度按"方面"打分，整体提速。** 聚焦爬取看页面是不是在"讲"查询的各个方面：查询词，加上本站
当作词组用的相邻查询词（如 *computer science*）；标题、网址、小标题比正文分量重；各方面按在本站的
稀有程度加权。所有统计量都从这次爬取自己抓到的页面里现估。在 1,833 个按正文判过的页面上，
没参与设计的那一半学校里，区分有用和无用的 AUC 从 0.79 升到 0.86。速度：按线程和 NUMA 节点的
小块分配缓存，PDF 字形文字内联存放，HTML 标题比对改成线性，CSV、xlsx 表格连续存放，发布版开全程序
LTO 和按剖析优化（PGO）；输出和 0.80.10 逐字节相同。详见 [0.81.0 发行说明](releases/0.81.0.md)。

## 0.80.10 更新

**页数上限变成自适应的。** `crawl --query … --adaptive` 不用再给 `--limit`：页面还在带来新的相关内容
就接着抓，不再带来时停；一直找不到相关内容时也停（`no_relevant_pages`）；上限只是保险（不给时 500）。
69 所大学、五个方向实测：正中查询的页面，自适应 4,208 页，0.80.9 的 `--limit 12` 是 645 页、
`--limit 100` 是 1,856 页，而且有用页的比例更高（25.7% 对 20.0% 和 17.2%）。发现：英文
查询不再被外语主站吃掉预算，`/about/housing/` 这类服务页不再当"关于"页。正文提取：新闻稿导语、
新闻侧栏、上传 HTML 的相对链接（`/v2/parse` 新增 `url` 字段）。详见 [0.80.10 发行说明](releases/0.80.10.md)。

## 0.80.9 更新

**0.80 的收尾版本。** 聚焦爬取扩大到 69 所大学、五个方向（国际生、研究生申请、住宿、计算机系、图书馆）实测：
对 0.80.0，正中查询的页面 2.6% → 19.0%，相关以上 18.4% → 49.6%；排队的链接随学到的词权重重新打分，
同一页的不同写法只交付一次。PDF 收进表单域的值、批注、图章和签名里的字（pdf.js 困难测试文件 100 份里 53 → 61 份）。
每份文件起一个进程时 `scrape` 启动更轻（每次 CPU 6.3 → 4.3 ms）。`doctor` 逐个启动引擎，起不来的会警告；
PATH 以外和 Playwright 缓存里的引擎也能找到；`crawl`、`map` 支持 `--ignore-robots`。手风琴、FAQ 的标题保留在正文里。
所有基准对 0.80.0 都在 2% 以内。没有发布 0.80.6–0.80.8。详见 [0.80.9 发行说明](releases/0.80.9.md)。

## 0.80.5 更新

**看起来正常、其实有问题的结果会明说了。** 结果可能不完整或不对时（PDF 丢页丢内容、正文过薄或只抽到一部分、
输出被截断、渲染只拿到壳……），警告会写进 Firecrawl 的 `data.warning`，按 Firecrawl 格式读的程序也能看到；
`crawl --out` 另写 `summary.json`。大站框架里的年龄门（Steam 的 `/agecheck/`）和标成 PDF 的 HTML 拦截页
报 `ACCESS_BARRIER`，不再当正文返回。Word、Excel、PowerPoint、CSV、EPUB 的输出到 16 MB 文字就停，并给出
`output_truncated`（以前 408 KB 的 DOCX 能展开成 84 MB Markdown）。PDF 损伤警告按文档真实页数计数，并点出只丢了
一部分的页。网络静默时页面还是空壳的，渲染最多再等 1.5 秒。`--engine`、`--format`、`--sitemap` 拼错会报错。
所有基准对 0.80.0 都在 2% 以内。没有发布 0.80.4。详见 [0.80.5 发行说明](releases/0.80.5.md)。

## 0.80.3 更新

**聚焦爬取在真实大学站点上验证过了。** 用 0.80.0 和 0.80.3 对同样 38 所大学首页跑
`crawl --query "international students admission tuition scholarship visa" --adaptive --limit 12`，
两版都能访问的 32 所上：有用页面 29.6% → 66.1%，国际生专属页面 6.3% → 38.9%，每校有用页中位数 1.5 → 8
（按 URL 和标题自动判定；脚本和原始结果在 `benchmarks/history/0.80.3/`）。带查询的爬取默认覆盖整站、失败的抓取
退还页面预算、通过 `hreflang` 和英文子域名找到英文站、不再把导航大菜单当列表。截断的 PDF 丢了某页的内容时
会指出是哪一页（`content_missing`）。详见 [0.80.3 发行说明](releases/0.80.3.md)。

## 0.80.2 更新

**聚焦爬取抓到的是要找的页面。** 一位测试者用 `crawl --query` 跑了 159 所大学官网（查询 *international students
admission tuition scholarship visa*，每站 12 页），有用页面只占 25.6%。这一版重写了链接打分、调度和停止规则：
每个链接都按锚文本打分（导航栏里的也算）、查询词按在站内的稀有程度加权、新闻和"关于"类页面降权、各栏目轮流出队、
子域名默认在范围内、错误页不算页面、自适应停止只拿相关页当证据。在复现报告里各种失败模式的 8 个合成站点上，
有用页面从 7.3% 提到 82.6%；**开发环境连不上真实大学站点，这些数字只来自合成站点。**
新增：`attachments` 抓取相关页面上链到的 PDF 和 Office 文档；每条结果带爬取轨迹（`depth`、`parent`、`score`、`attachmentOf`）。
PDF 两项补修：书签只到章节时标题不再为空；线性化 PDF 截断到页树根之前时照常转换而不是报错。
详见 [0.80.2 发行说明](releases/0.80.2.md)。

## 0.80.1 修复

0.80 复查发现的问题（网页提取代码未动；PDF 输出只在词后上标脚注标记处有变化）：Shift_JIS、GBK 或任何声明了字符集的 CSV 按网页同样的方式解码（之前整页乱码且无提示）；
损坏的 PDF 只恢复出一部分时会说明（`pdf_repaired`、`pages_missing`）；作者名后的脚注标记不再写成公式
（`Ashish $Vaswani^{\ast}$` → `Ashish Vaswani*`）；没有内嵌标题的 PDF 把推断的标题回填到 `metadata.title`
（`titleSource: inferred`）；`*.localhost` 在任何机器上都解析到回环地址，测试和性能门在纯 IPv4 机器上也能跑；
`NOTICE`（补上 Adobe cmap-resources 的许可）随每种二进制分发。详见 [0.80.1 发行说明](releases/0.80.1.md)。

## 0.80.0 新增内容

**PDF 保留原有结构，结果会说明自己可能漏了什么。**

- **自研 PDF 文件层**替换 lopdf：xref、对象流、解码、加密和坏文件修复都自己做，更快、更省内存。
- **PDF 标题带真实层级**：优先读写入方给的结构树和书签，没有时按编号、字号、粗体推断。AiSeek 报告的 13 份 PDF 上（越高越好）：召回 0.595 → 0.947，精确率 0.400 → 0.876，层级一致率 0.819 → 0.957，空标题 3 → 0。
- **页眉页脚与图表**：识别 `/Artifact` 标记、带页码的页眉页脚和矢量图。olmOCR-bench `headers_footers` 0.681 → 0.734（越高越好）；13 份 PDF 上误判的表格 196 → 9（越低越好）。
- **CJK text without ToUnicode**: Chinese, Japanese and Korean PDFs whose CID fonts carry no
  `/ToUnicode` map (LaTeX/ctex with Fandol fonts, many others) lost all CJK text; it is now decoded
  through the Adobe-GB1/CNS1/Japan1/Korea1 collections or the embedded TrueType cmap. On 19 diverse
  Chinese PDFs, inferred-heading recall 0.497 → 0.826 (higher is better).
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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.3
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
