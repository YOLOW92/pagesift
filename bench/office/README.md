# Office / EPUB / CSV 转 Markdown 评测（0.80）

对比 pagesift（`pagesift-office`）、MarkItDown 0.1.9 和 pandoc 3.9 把 Word、Excel、PowerPoint、EPUB、CSV
转成 Markdown 时保住了多少结构（越高越好），以及耗时、峰值内存（越低越好）。
真值怎么标、评分细则、留出集规则见 [REVIEW.md](REVIEW.md)（收集留出集之前定稿）；结果见 `results-*.md`。

## 目录

- `sources/dev.tsv`、`sources/holdout.tsv`：每份文件的来源网址、许可、sha256。语料文件本身不随仓库保存
  （许可各异，也太大），用下面的脚本重新下载，sha256 应当一致（GOV.UK 的附件可能更新）。
- `truth/dev/`、`truth/holdout/`：真值（JSON，每个文件一份）。
- `scripts/`：下载、打包、标注、转换、评分、计时、内存的脚本。

## 复现

在一个空的工作目录里（脚本都用相对路径 `dev/`、`holdout/`、`truth/`、`out/`）：

```bash
# 1. 语料：POI、csv-spectrum、datasets、Gutenberg 按清单下载；GOV.UK 按查询词规则挑；IDPF 样本稀疏克隆后打包
python3 -I scripts/fetch.py scripts/dev.list
python3 -I scripts/govuk.py docx "guidance template" 3 dev/docx 2000 20   # 其余查询词见 REVIEW.md 和 sources/*.tsv
python3 -I scripts/pack_epub.py <epub3-samples>/30/accessible_epub_3 dev/epub/idpf-accessible_epub_3.epub
# 2. 真值（需要 pandoc 3.9、python-docx、python-pptx 的 venv，以及 LibreOffice Calc）
scripts/make_truth.sh dev
# 3. 转换
scripts/run_pagesift.sh <pagesift 二进制> dev out/dev/pagesift
~/.venvs/markitdown/bin/python -I scripts/run_markitdown.py dev out/dev/markitdown
scripts/run_pandoc.sh dev out/dev/pandoc
# 4. 评分（越高越好）
python3 -I scripts/score.py truth/dev out/dev/pagesift
# 5. 耗时（进程内，每个文件 5 次取中位数）和峰值内存（每个文件单独一个进程）
cargo run --release -p pagesift --example office_bench -- 5 dev/*/*.*
~/.venvs/markitdown/bin/python -I scripts/time_markitdown.py 3 dev/*/*.*
scripts/mem.sh <pagesift 二进制> dev holdout
```

测速前先等机器空闲（`tasks/0.80/wait_idle.sh`：1 分钟负载连续 3 分钟低于 1.5）。
