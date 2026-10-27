# 参评工具版本锁定与安装方法（混合增强基准）

核实日期 2026-10-27。名单见 [RULES.md](RULES.md) 第四节。全部只从官方源安装（PyPI、npm、crates.io、
PyTorch 官方 wheel 源、项目官方 GitHub 发布页），装在仓库外的独立目录 `~/mix-tools`（下文 `$MIX`），默认参数。
不做 Docker 镜像。适配器和调度见 [adapters/README.md](adapters/README.md)。

## 机器

| 项目 | 值 |
|---|---|
| CPU | Intel Core i7-10700 @ 2.90GHz，8 核 16 线程 |
| 内存 | 15 GB，无 swap 参与（每个 worker 设 `MemorySwapMax=0`） |
| GPU | NVIDIA GeForce GTX 1070 8 GB（Pascal，计算能力 6.1），驱动 580.178.04 |
| 系统 | Linux 7.0.0-38-generic，systemd 259（cgroup v2） |
| 运行时 | CPython 3.12.15（uv 管理）、Node.js 24.21.0、rustc 1.99.0 |

## 版本总表

| 领域 | 工具 | 版本 | 许可证 | 语言 | 来源 | 本基准的输入 | 输出 | 内部并行（默认） |
|---|---|---|---|---|---|---|---|---|
| 网页正文 | crw（crw-extract） | 0.39.0 | AGPL-3.0 | Rust | crates.io `crw-extract`、`crw-core` | HTML 文件 | Markdown | 单线程 |
| 网页正文 | trafilatura | 2.3.1 | Apache-2.0 | Python | PyPI | HTML 文件（原始字节） | Markdown | 单线程 |
| 网页正文 | Mozilla Readability + Turndown | 0.6.0 + 7.2.4（jsdom 30.1.3） | Apache-2.0 / MIT / MIT | JavaScript | npm | HTML 文件 | Markdown | 单线程 |
| 抓取 | spider（spider-rs） | 2.53.9（spider_transformations 2.39.13） | MIT | Rust | crates.io `spider` | 回放服务上的 URL | Markdown | tokio 多线程运行时，线程数 = 核数 |
| 抓取 | Scrapy | 2.19.0（Twisted 26.4.0） | BSD-3-Clause | Python | PyPI | 回放服务上的 URL | 解码后的 HTML（Scrapy 不转 Markdown） | 单线程事件循环 |
| 抓取 | Crawl4AI | 0.9.5（Playwright 1.64.1，Chrome Headless Shell 156.0.8078.4） | Apache-2.0 | Python | PyPI；浏览器由 Playwright 官方下载 | 回放服务上的 URL | Markdown（`result.markdown`） | 每个 worker 一个无头 Chromium（多进程） |
| PDF | pdf_oxide | 0.3.78 | MIT OR Apache-2.0 | Rust | crates.io `pdf_oxide` | PDF 文件 | Markdown | 单线程 |
| PDF | pymupdf4llm | 2.0（代码在 PyMuPDF 2.0 里） | AGPL-3.0（或 Artifex 商业许可） | Python + C | PyPI `pymupdf==2.0`、`pymupdf4llm==2.0` | PDF 文件 | Markdown | 单线程 |
| PDF | Docling | 2.142.1（docling-parse 7.23.1，docling-ibm-models 4.0.4，torch 2.14.1） | MIT | Python | PyPI；torch 的 GPU 版见下 | PDF 文件 | Markdown | torch 线程数默认 = 核数 |
| Office 与电子书 | extractous | 0.3.0（内嵌 Apache Tika，GraalVM CE 23.0.1 原生编译） | Apache-2.0 | Rust | crates.io `extractous`；构建时从 graalvm/graalvm-ce-builds 官方发布页下 GraalVM | docx/xlsx/pptx/epub/csv 文件 | 纯文本（extractous 不出 Markdown） | Tika 内部线程 |
| Office 与电子书 | MarkItDown | 0.1.9（`markitdown[all]`） | MIT | Python | PyPI | 同上 | Markdown | 单线程 |
| Office 与电子书 | Docling | 同 PDF 一行 | MIT | Python | 同上 | docx/xlsx/pptx/epub/csv | Markdown | 同上 |

Rust 项目都没有换候选：crw、spider、pdf_oxide、extractous 都装得上，也都支持对应领域的输入。

## 安装（别人照着能复现）

前提：`uv`（或任意 CPython 3.12 + pip）、Node.js ≥ 20、Rust 稳定版、`perl` 和 `make`（编 vendored OpenSSL 用）。
下面在仓库根目录执行；锁文件都在 `bench/mix/locks/`。

### Python 工具（每个工具一个 venv，带哈希锁定）

锁文件由 `uv pip compile --generate-hashes --python-version 3.12 --python-platform x86_64-manylinux_2_28 <工具>.in`
生成；`.in` 里只写顶层包和版本，`.txt` 是含全部传递依赖和哈希的锁。

```bash
MIX=~/mix-tools
for t in trafilatura scrapy crawl4ai pymupdf4llm docling markitdown extractous; do
  uv venv -p 3.12 $MIX/py/$t
  VIRTUAL_ENV=$MIX/py/$t uv pip sync --require-hashes bench/mix/locks/py/$t.txt
done
# 不用 uv 时：python3.12 -m venv $MIX/py/$t && $MIX/py/$t/bin/pip install --require-hashes -r bench/mix/locks/py/$t.txt
```

- **Crawl4AI 的浏览器**：`PLAYWRIGHT_BROWSERS_PATH=$MIX/ms-playwright $MIX/py/crawl4ai/bin/python -m playwright install chromium`
  （官方的 `crawl4ai-setup` 做的是同一件事，但它在下载中断时也返回 0，本机就碰到过，所以直接调 Playwright）。
- **Docling 的模型**：第一次转换时从 Hugging Face 下（docling 官方模型仓库），driver 设 `HF_HOME=$MIX/hf`。
- **Docling 的 GPU 版**（附表用，只为 GTX 1070 这类旧卡；olmOCR 固定 20 页实测比 CPU 快 8.3 倍，见
  `results/docling-gpu-check-2026-10-27.json`）：PyPI 上的 torch 2.14.1 是 CUDA 13.0 版，只带 sm_75 及以上
  的内核，GTX 1070（sm_61）上 `torch.cuda.is_available()` 返回 True 但一跑就报 `no kernel image is available`。
  同一版本 torch 的 CUDA 12.6 版来自 PyTorch 官方 wheel 源：

  ```bash
  uv venv -p 3.12 $MIX/py/docling-cu126
  VIRTUAL_ENV=$MIX/py/docling-cu126 uv pip sync --require-hashes \
      --extra-index-url https://download.pytorch.org/whl/cu126 --index-strategy unsafe-best-match \
      bench/mix/locks/py/docling-cu126.txt
  ```

| venv | 锁文件 | 包数 | 占用 |
|---|---|---|---|
| trafilatura | locks/py/trafilatura.txt | 18 | 62 MB |
| scrapy | locks/py/scrapy.txt | 44 | 70 MB |
| crawl4ai | locks/py/crawl4ai.txt | 95 | 686 MB（另加浏览器 约 300 MB） |
| pymupdf4llm | locks/py/pymupdf4llm.txt | 12 | 244 MB |
| docling | locks/py/docling.txt | 122 | 5.9 GB（含 CUDA 13 运行库） |
| docling-cu126 | locks/py/docling-cu126.txt | 122 | 7.0GB（含 CUDA 12.6 运行库） |
| markitdown | locks/py/markitdown.txt | 49 | 329 MB |
| extractous（备用） | locks/py/extractous.txt | 1 | 115 MB |

### Node 工具

```bash
mkdir -p ~/mix-tools/node/readability
cp bench/mix/locks/node/package.json bench/mix/locks/node/package-lock.json ~/mix-tools/node/readability/
(cd ~/mix-tools/node/readability && npm ci)
```

### Rust 工具（独立 Cargo 工作区 `bench/mix/adapters/rust`，`Cargo.lock` 锁全部依赖）

```bash
cd bench/mix/adapters/rust
CARGO_TARGET_DIR=~/mix-tools/rust/target cargo build --release --locked \
    -p mix-crw -p mix-spider -p mix-pdf_oxide -p mix-extractous
mkdir -p ~/mix-tools/rust/bin
cp ~/mix-tools/rust/target/release/mix-{crw,spider,pdf_oxide,extractous} ~/mix-tools/rust/bin/
cp -r ~/mix-tools/rust/target/release/build/extractous-*/out/libs ~/mix-tools/rust/bin/libs   # Tika 原生库
rm -rf ~/mix-tools/rust/target   # 中间产物约 3.6 GB，可删
```

- spider 的默认特性经 sqlx 用到 native-tls → openssl-sys；extractous 的 build.rs 下载 GraalVM 时也用。本机没有
  OpenSSL 开发包，两个适配器都加了 `openssl = { features = ["vendored"] }`（含 build-dependencies，因为 resolver 2
  下宿主侧依赖的特性单独计算）。只影响链接方式，不改工具行为。
- extractous 的 build.rs 会自己下 GraalVM CE 23.0.1（约 1 GB 解压后）并用 Gradle 编 Tika 原生库；本机约 3 分钟，
  套了 8 GB 内存上限没有超。也可以设 `GRAALVM_HOME` 用已装的 GraalVM。
- 运行 extractous 需要 `LD_LIBRARY_PATH=~/mix-tools/rust/bin/libs`（driver 已设）。

## 默认参数与唯一的统一改动

全部工具用默认参数，只有一条统一改动：**能出 Markdown 的就要 Markdown**（本基准比的就是转 Markdown）。

| 工具 | 调用 | 和默认值不同的地方 |
|---|---|---|
| crw | `crw_extract::extract`，formats = [Markdown] | `only_main_content = true` 本来就是 crw 服务端默认 |
| trafilatura | `trafilatura.extract(原始字节, output_format="markdown", url=原始 URL)` | 默认 `txt` → `markdown`；默认 `include_links=False`，所以输出里没有链接（照默认） |
| Readability + Turndown | `new Readability(jsdom.document).parse()` → `new TurndownService().turndown(content)`，标题拼在前面 | 无（Turndown 默认标题是 setext 风格） |
| spider | `Website::new(url).with_limit(1)`，subscribe + crawl；`transform_content(TransformConfig{return_format: Markdown, ..Default})` | 默认返回原始 HTML → Markdown；限一页 |
| Scrapy | `CrawlerRunner()` 默认设置，一个只抓一页的 spider，输出 `response.text` | 无 |
| Crawl4AI | `AsyncWebCrawler()` 默认 BrowserConfig，`arun(url)` 默认 CrawlerRunConfig，取 `result.markdown` | 无 |
| pdf_oxide | `PdfDocument::open` → `to_markdown_all(&ConversionOptions::default())` | 无 |
| pymupdf4llm | `pymupdf4llm.to_markdown(path)` | 无 |
| Docling | `DocumentConverter().convert(path).document.export_to_markdown()` | 设备用 `DOCLING_DEVICE` 指定（cpu / cuda），见上 |
| extractous | `Extractor::new().extract_file_to_string(path)` | 无（只出纯文本） |
| MarkItDown | `MarkItDown().convert(path).text_content` | 无（默认不启用插件） |

## 冒烟测试结果（2026-10-27，`bench/mix/smoke.sh`，不是正式分数）

见 RULES.md 第六、七节记下的问题。逐工具结果在下表；耗时、内存只说明“跑得通”，不能当成绩用。

样本：网页快照 3 份（bench/data/snapshots 1、10、100）、olmOCR PDF 3 份（tables、multi_column、arxiv_math 各 1）、
Office 4 份（heading123.docx、table_test.pptx、ExcelTables.xlsx、gutenberg-1342.epub）。抓取工具读同一批网页快照的回放 URL。
单份上限 300 s，单 worker 内存上限 4 GB；worker 数由 driver 按 MemAvailable 自动定。docling 一行是模型已缓存后的复测
（第一次跑时模型现下，两个 worker 同时加载，一份报 `storage has wrong byte size`，见 RULES 第七节第 4 条）。
`docling-auto` 是纯默认（不指定设备），在 GTX 1070 上全部失败，见 RULES 第七节第 3 条。

| 工具 | 版本 | 份数 | 通过 | worker 数 | 启动（ms） | 单份耗时中位数（ms，越低越好） | 单份峰值内存最大（MB，越低越好） | 没通过的 |
|---|---|---|---|---|---|---|---|---|
| crawl4ai | 0.9.5 | 3 | 3 | 3 | 1591 | 1450.2 | 393.0 | — |
| crw | crw-extract 0.39.0 | 3 | 3 | 3 | 68 | 36.1 | 2.6 | — |
| docling-office | 2.142.1 | 4 | 3 | 2 | 4026 | 29.2 | 544.9 | 00001-table_test：empty |
| docling | 2.142.1 | 3 | 3 | 2 | 3477 | 5725.4 | 1708.1 | — |
| extractous-py | 0.3.0 | 4 | 4 | 4 | 355 | 655.8 | 99.1 | — |
| extractous | extractous 0.3.0 | 4 | 4 | 4 | 70 | 256.4 | 69.2 | — |
| markitdown | 0.1.9 | 4 | 4 | 4 | 4838 | 49.7 | 141.8 | — |
| pdf_oxide | pdf_oxide 0.3.78 | 3 | 3 | 3 | 68 | 49.1 | 10.5 | — |
| pymupdf4llm | pymupdf 2.0 | 3 | 3 | 3 | 3274 | 537.0 | 296.4 | — |
| readability | @mozilla/readability 0.6.0 + turndown 7.2.4 + jsdom 30.1.3 | 3 | 3 | 3 | 1773 | 1004.9 | 225.6 | — |
| scrapy | 2.19.0 | 3 | 3 | 3 | 376 | 328.2 | 60.3 | — |
| spider | spider 2.53.9 | 3 | 3 | 3 | 44 | 9.0 | 5.6 | — |
| trafilatura | 2.3.1 | 3 | 3 | 3 | 988 | 421.8 | 60.4 | — |
| docling-auto | 2.142.1 | 3 | 0 | 2 | 3711 | — | 790.0 | 00001-0005784d0d255f6652180433936fa2998188_page_1_pg1：error（ConversionError: Conversion failed for: 0005784d0d255f665218）；00000-008d1dbe3c5a5fd2bac6e4f29acc45e711aa_pg8_pg1：error（ConversionError: Conversion failed for: 008d1dbe3c5a5fd2bac6）；00002-2502.15977_pg21：error（ConversionError: Conversion failed for: 2502.15977_pg21.pdf ） |
| docling-gpu | 2.142.1 | 3 | 3 | 2 | 8195 | 12503.1 | 1823.2 | — |

