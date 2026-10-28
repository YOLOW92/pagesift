# 混合增强基准（Mix）：规则（第 2 版，2026-10-28 冻结）

第 2 版在第 1 版（[../RULES.md](../RULES.md)，冻结点 f1a0cdf3）的基础上加竞品、加类别，并在本机重测
pagesift 0.80.9。第 1 版的规则、样本和结果原样保留；本文件只写和第 1 版不同的地方，没写到的一律照第 1 版。

状态：**第 2 版，已冻结**。冻结点就是把本行和 `seeds.toml` 的 `frozen = true` 一起提交的那个提交号
（`git log -1 --format=%H -- benchmarks/mix/v2/seeds.toml`）。冻结之后才抽新类别的样本、才跑；之后规则、样本、
评分都不再改，要改就另起第 3 版。第 1 版的六条硬规则（先定规则再抽样、抽样不挑、弱项照样放、不靠人工标注、
一视同仁、全部公开）全部沿用。

---

## 一、和第 1 版相比改了什么

1. **重测对象**：pagesift 0.80.9（提交 33ff97ab 按发布工作流的命令 `cargo build --release --locked -p pagesift`
   编出的二进制），另在同一台机器上重跑 0.80.0 的发布二进制（GitHub 发布页的 x86_64 Linux 包）作为基线。
   第 1 版的 0.80.0 数字和这次的 0.80.0 数字都发布，用来看机器状态是否一致。
2. **原有类别沿用第 1 版的样本**：A1、A2、A4、B1、B2、B3、B4、C2 用第 1 版冻结的样本清单（`../samples/`）和
   同一批文件（`~/mix-tools/inputs`），种子、配额、评分、时间上限都不变。原有工具也用第 1 版锁定的版本重装
   （`../locks/`），这样 0.80.0 → 0.80.9 的变化只来自 pagesift。
3. **新加竞品**（第四节）和**新加类别**（第二节）：A3 需要 JS 渲染的网页、B5 扫描件 PDF、D 定向爬取；
   C3 长时间运行第 1 版写了但没跑，这一版跑。
4. **分三个阶段跑，从快到慢**（第三节）。每个阶段结束就出报告、提交一次，后面的阶段不回头改前面阶段的数字。
5. **pagesift 加一种运行方式**：第 1 版 pagesift 每份起一个 CLI 进程，别的工具是常驻 worker，单份耗时里
   pagesift 多算了进程启动。这一版两种都报：`pagesift`（每份一个进程，和第 1 版可比）和 `pagesift-serve`
   （常驻的 `pagesift serve`，每份一次本机 HTTP 请求，算作第 1 版第 5 条允许的“官方推荐配置”）。

## 二、新类别

| 子类 | 来源与抽样 | 数量 | 评什么 |
|---|---|---|---|
| A3 需要 JS 渲染的网页 | Tranco 前 100 万，按第 1 版 A2 的 5 层分层，用 A3 的种子逐层抽候选站点首页，按种子顺序判断，直到每层 20 份合格。**合格**只看静态抓到的原始 HTML（不看任何参评工具的输出）：去掉 `script`/`style`/`noscript`/`template` 后的可见文字少于 500 字，且至少有 3 个外部脚本（`<script src>`）或一个常见前端框架的挂载点（`id` 为 `root`、`app`、`__next`、`__nuxt`、`svelte` 的空元素）。静态抓取遵守 robots.txt，UA 写仓库地址 | 100 | 实时抓（渲染页没法做成能复现的快照，所以不回放）：所有工具在同一个时间段内依次跑，各自拿到的内容都存档。**参照文本**由不参评的 Playwright **Firefox** 渲染两次（所有工具跑之前一次、之后一次），只取两次都出现的词，排除轮播、时间这类会变的内容。评：完成率（输出 ≥ 200 字）、参照词覆盖率、耗时、峰值内存 |
| B5 扫描件 PDF | olmOCR-bench 的 `old_scans`（98 页）和 `old_scans_math`（36 页），全部，不抽样 | 134 页 | 沿用 olmOCR 评分；完成率、耗时、峰值内存。**pagesift 没有 OCR，这一项预期垫底**，照样发布 |
| D 定向爬取 | 学校名单取公开的 Hipo `university-domains-list`（GitHub，记下提交号和文件 SHA-256），**去掉 pagesift 开发时用过的 69 所**（`benchmarks/history/0.80.9/sites.txt`），用 D 的种子抽 30 所；方向沿用 0.80.9 的 5 个（intl、grad、housing、cs、library，查询词见 `benchmarks/history/0.80.9/README.md`） | 30 所 × 5 个方向 | 实时爬，每个“学校 × 方向”每个工具 12 页预算；**所有工具都遵守 robots.txt**。评分用 0.80.9 的 `grade.py`（按 URL 和标题判 U/M/X，规则对所有工具一样）：U 占比、U+M 占比、一页都没命中 U 的“学校 × 方向”数。首页打不开的学校照实记“不可用”，不换 |
| C3 长时间运行 | 服务形态的工具（`pagesift serve`、Firecrawl 自托管）把 A2 的 269 页连续处理 3 遍 | — | 每遍的错误率、内存曲线（每 5 秒采一次 cgroup `memory.current`）是否持续增长 |

D 的名单去掉开发集，是因为 0.80.2–0.80.9 的定向爬取规则是对着那 69 所学校调的；在它们上面比，等于用训练集考试。

## 三、三个阶段

按“跑起来快慢”排，快的先出结果。阶段内部仍然一项接一项走 `bench.lock`；只在等网络、不占 CPU 的事
（下载模型、拉 Docker 镜像）可以和测速并行。

| 阶段 | 工具 | 类别 |
|---|---|---|
| 一（快） | 不用模型、不开浏览器的工具：pagesift 0.80.9 / 0.80.0 / serve、crw、trafilatura、Readability、Resiliparse、newspaper4k、spider、Scrapy、Crawlee、pdf_oxide、pymupdf4llm、Kreuzberg、MinerU（flash 档）、extractous、MarkItDown、pandoc | A1、A2、A4、B1、B2、B3、B4，C2（1/4/16 路） |
| 二（中） | 用模型的工具：MinerU（默认档）、Marker、Docling（CPU 主成绩 + GPU 附表）、MinerU-HTML；开浏览器的工具：Crawl4AI、Firecrawl 自托管、pagesift 的浏览器引擎 | B1、B2、B3、B4 的 10% 模型子集（第 1 版的 `model_subset` 列），A1 的 10% 模型子集（MinerU-HTML），A2 抓取（Crawl4AI、Firecrawl），A3 |
| 三（慢） | 同上 | B5、D、C3 |

## 四、参评工具（第 2 版）

每个领域在第 1 版三家的基础上加新竞品。版本在装的时候锁定，写进 `tools.lock.md`；全部默认参数，统一改动
仍然只有“能出 Markdown 的就要 Markdown”。

| 领域 | 第 1 版已有 | 第 2 版新加 |
|---|---|---|
| 网页正文 | crw、trafilatura、Readability + Turndown | Resiliparse（`extract_plain_text(main_content=True)`，只出纯文本）、newspaper4k（只出纯文本）、MinerU-HTML（用模型，只跑 A1 的 10% 子集） |
| 抓取 | spider、Scrapy、Crawl4AI | Crawlee（Python，`HttpCrawler`，不开浏览器，输出原始 HTML）、Firecrawl 自托管（官方 docker compose，默认配置） |
| PDF | pdf_oxide、pymupdf4llm、Docling | **MinerU**（默认安装、默认档）、Marker、Kreuzberg；另报 MinerU 的 flash 档（官方文档里的“最快、不用模型”档，算“官方推荐配置”，表里注明） |
| Office 与电子书 | extractous、MarkItDown、Docling | Kreuzberg、pandoc、MinerU（Office/EPUB/CSV 官方只走 flash 档，这就是它的默认） |
| 定向爬取（D） | — | Crawl4AI 的 `AdaptiveCrawler`（`digest(start_url, query)`，默认策略，`max_pages = 12`，打开它的 robots.txt 检查以和 pagesift 一致） |

- **GPU**：GTX 1070（sm_61）。Docling、Marker 的默认安装（PyPI 上的 CUDA 13 版 torch）跑不了这张卡，主成绩用
  CPU，另用 PyTorch 官方源的 cu126 版出 GPU 附表（同第 1 版的 Docling 做法）。MinerU 默认安装的 VLM 走
  llama.cpp 的 Vulkan 后端，本来就会用上这张卡，照默认跑，表里写明用到了 GPU。
- **模型子集**：用模型的工具只跑第 1 版 `model_subset = 1` 的份（每类 10%），B5 例外：B5 只有 134 页，全跑。
- **装不上、跑不了的工具**照实写进报告和 `tools.lock.md`（原因、试过什么），不替换成别的工具。

## 五、时间上限（`limits.toml`）

原有类别沿用第 1 版。新类别：A3 单份 60 s（含渲染）；B5 单份 600 s；D 每个“学校 × 方向”600 s；C3 单份 30 s。
单个 worker 内存上限仍是 4 GB；用模型的工具放宽到 8 GB（第 1 版 Docling 单 worker 峰值 3.5 GB，MinerU、Marker
预计更高），表里注明。

## 六、发布方式

- 报告 `v2/results/report-<日期>.md`，三个阶段各一节；每个数字标明越高越好还是越低越好，并链到结果文件。
- README 最上端加“性能一览”：只列 pagesift 领先的速度和内存项，每行写明最快的对手、倍数和证据链接；
  输的项留在下面“How PageSift compares”的“Behind”栏里，照第 1 版第五节，输的照样写进 README。
- 每个阶段结束提交一次（报告、结果、README 四种语言、`docs/benchmarks.md`），不改版本号、不打标签。

## 七、预期（冻结时填写，之后不改）

- **阶段一速度**：0.80.9 的轻量启动（发布说明：每次调用 CPU 6.3 → 4.3 ms）让每份一个进程的 pagesift 比 0.80.0
  快一截，16 路 PDF 吞吐的差距（第 1 版 142 对 pdf_oxide 526）缩小但仍落后；`pagesift-serve` 在 16 路上追平或
  超过 pdf_oxide。Resiliparse 在正文提取单份耗时上和 pagesift、crw 同一量级，可能更快。
- **阶段一质量**：A1 和第 1 版持平（0.80.x 的正文提取只加了手风琴/FAQ 标题，Firecrawl 集召回略升）；B3 困难
  PDF 完成率从 0.560 升到 0.6 以上（0.80.9 的注释外观和 Type3 修复）；B4 结构保留率仍然领先，Kreuzberg、
  pandoc、MinerU 介于 MarkItDown 和 pagesift 之间。Resiliparse、newspaper4k 只出纯文本，F1 会吃亏。
- **阶段二**：MinerU、Marker、Docling 在 B1 olmOCR 分数上至少有一个超过 pagesift（版面模型的强项是表格和阅读
  顺序）；速度上 pagesift 比它们快两到三个数量级。A3 上 pagesift（auto 引擎）和 Crawl4AI、Firecrawl 的覆盖率
  同一水平，耗时和内存更低。
- **阶段三**：B5 上 pagesift 垫底（没有 OCR）；D 上 pagesift 的 U 占比高于 Crawl4AI 的自适应爬取；C3 上 pagesift
  三遍内存不持续增长。
