# 新文档格式评测：真值标注规则、评分规则、留出集规则（2026-10-27 定稿）

这份文件在收集留出集之前写完，之后不改规则、不按留出集结果调任何阈值。

## 一、真值怎么来的（make_truth.sh）

1. 候选：pandoc 3.9（独立实现，`label.py`）把每个文件转成 HTML，取标题、表格（行列数和 4 个抽样单元格）、
   列表项、图片替代文字、脚注、正文样句；pptx 的备注和图片说明用 python-pptx 读。
2. 按格式换成更可靠的来源（规则统一，不针对单个文件）：
   - docx 标题与列表（`label_docx.py`，python-docx 读样式）：段落或样式继承链上有 outlineLvl（默认段落样式上的不算），
     或内置样式名 `heading N`/`Title`，或自定义样式名含 Heading/Subheading/Title（Subtitle、TOC、Caption、Table 除外）；
     列表项是带编号属性且不是标题的段落。原因：pandoc 只认内置标题样式，漏了 “Heading 1 (Agency)” 这类继承自内置
     标题的样式；它还把带编号的标题当列表。
   - xlsx 表格（`label_xlsx.py`）：LibreOffice Calc（en-US 区域）导出的 HTML，单元格是按数字格式渲染的显示值；
     合并单元格展开；尺寸是去掉首尾全空行列后的范围。原因：pandoc 给的是原始数值（`8.7e-2`），不是用户看到的值。
     LibreOffice 导出不含隐藏的行列。
   - epub（`label_epub.py`）：按 OPF spine 直接读 XHTML，标题只取文字节点（不含图片替代文字）。原因：pandoc 把图片说明
     和章节标题合在一起。
3. 人工核对后的统一修正（`edits_common.py`）：去掉 1×1 或没有内容的表格（排版用的表格、EPUB 里放公式的表格）；
   pptx 去掉 pandoc 编出来的 “Slide N” 标题和 “[Graphic: …]” 占位文字，列表项和样句换成 python-pptx 读到的原文
   （pandoc 会在两个文字片段之间插空格，如 “w hat”）；各类元素去重。
4. 抽查：每份文件的候选都看过；对照原始 XML 核对过图片 descr、标题样式、隐藏行列、Symbol 字体字符等可疑处。

## 二、评分（score.py）

结构保留率（越高越好）= 保住的真值元素数 / 真值元素总数，元素类别：标题、表格、抽样单元格、列表项、图片替代文字、
脚注、备注、标题顺序。正文召回（越高越好）另报。判定细则写在 score.py 文件头。

**评分修正（2026-10-27，在留出集之前）**：最初的规则只拿“去掉末尾全空行列后”的输出表尺寸去比，而 docx、pptx
的真值尺寸没有去空行空列（表单模板里的待填空行），把尺寸正确的表判成不符。改成：按原样或去掉末尾全空行列后，
任一相符即可。这条修正对所有工具一样；修正前后的开发集分数都留着（MarkItDown 0.820 → 0.825，pandoc 0.643 → 0.647）。

已知的评分局限：pandoc 把复杂表格输出成 HTML `<table>`，score.py 只认管道表，所以 pandoc 的表格分偏低；
pandoc 只作参考，主要对比对象是 MarkItDown。

## 三、留出集的收集规则（写死在这里，收集之后才转换、才评分）

来源和开发集不重叠；每种格式 5–8 份；文件清单在收集前定下：

- docx：Apache POI test-data `Bug51170.docx`、`bug65649.docx`、`issue_51265_1.docx`、`drawing.docx`、`endnotes.docx`、
  `form_footnotes.docx`；GOV.UK 查询词 “consultation document”，取前 3 份 40 KB–3 MB 的 .docx（govuk.py 规则）。
- xlsx：POI `ConditionalFormattingSamples.xlsx`、`ElapsedFormatTests.xlsx`、`DateFormatNumberTests.xlsx`、
  `57893-many-merges.xlsx`、`Booleans.xlsx`；GOV.UK 查询词 “statistics workbook”，前 3 份 20 KB–1.5 MB。
- pptx：POI `aascu.org_hbcu_leadershipsummit_cooper_.pptx`、`alterman_security.pptx`、`aptia.pptx`、`pie-chart.pptx`、
  `table-with-theme.pptx`、`SmartArt.pptx`；GOV.UK 查询词 “slide deck”，前 2 份 30 KB–5 MB。
- epub：IDPF epub3-samples `moby-dick`、`cc-shared-culture`、`jlreq-in-english`、`israelsailing`；
  Project Gutenberg #11、#84（epub3.images）。
- csv：csv-spectrum `json.csv`、`location_coordinates.csv`、`newlines.csv`、`simple_crlf.csv`；
  datasets `population/data/population.csv`、`gdp/data/gdp.csv`；GOV.UK 查询词 “data csv”，前 2 份 2 KB–1 MB。

真值用同一套流程（make_truth.sh holdout，edits_holdout.py 只调用 edits_common.py，不加针对单个文件的修改）。

## 四、预期（收集留出集之前写下）

- pagesift 在留出集上的结构保留率 ≥ 0.90，且高于 MarkItDown；开发集 0.98 与留出集的差距预计 3–8 个百分点
  （开发集上修过的通用问题在留出集上不一定覆盖新情况）。
- 预计较弱的地方：EPUB 里一列的表格（HTML 提取器把单列表格转成段落，这是 pagesift-content 的行为，本轮不改）、
  数据透视表的数字格式（没实现 pivotTable 定义里的格式）、SmartArt 文字（没实现）。

## 五、留出集评分之后的修正（2026-10-27，全部对所有工具一样，修正前后的分数都列在 results 文件里）

看留出集的丢分时，逐条对照原始文件，查出几处真值和评分本身的错误（不是 pagesift 的问题）。
这几处都按原文件可以验证，规则统一，不针对单个文件；修正前的 truth、score.py 留在任务目录里备查。

1. **pptx 真值：标题、表格单元格里 pandoc 插的空格**（`edits_common.py` 第 2 条补全）。原来的规则只把列表项和样句
   换成 python-pptx 读到的原文，标题和单元格漏了。例：原文是 “Trend” “: ” 两个文字片段，真值写成 “Trend : ”。
   用 python-pptx 逐条核对，开发集 0 处、留出集 5 处（4 个标题、1 个单元格）。
2. **xlsx 真值：表的尺寸改从原始工作表 XML 算**（`label_xlsx.py`）。原来从 LibreOffice 导出的 HTML 排版反推尺寸，
   而 LibreOffice 把图片画成跨行跨列的单元格（ConditionalFormattingSamples 的 Regional sales：一个跨 7 列 33 行的图片格），
   后面的数据被挤到第 9 列；原文件的范围是 A2:B17（2 列）。现在尺寸 = 有内容、没隐藏的单元格的行列范围（扣掉范围内
   隐藏的行列），显示值仍用 LibreOffice 的。重新生成后，开发集、留出集所有 xlsx 表里只有这 2 张的尺寸变了，其余全部
   和原来一致。（中途试过“只放图片的空单元格不占格子”，在 App_Reg_V3.5 上反而算错一列，弃用。）
3. **评分：Markdown 的写法等价时按同一个算**（`score.py`）：
   - 行尾反斜杠的硬换行和 `<br>` 一样当空格；
   - 列表项的续行、续段（缩进到列表项文字位置的行，CommonMark 的列表项规则）算在这一项里，
     原来只取了列表项的第一行；
   - 表格里的图片替代文字、HTML `<img alt>` 也算图片（原来只认表格外的 `![]()`，pandoc 的 HTML 表格里的图片全算丢了）；
   - 脚注引用 `[^1]` 按显示出来的 `1` 比较；
   - docx 标题级别超过 6 的，按 Markdown 最多 6 级比较。

同时本轮修了 pagesift 的几处通用问题（SmartArt、pptx 标题里原文的星号、嵌套表格、列表项续行缩进、
Symbol 字体未定义码位、数据透视表数字格式），见 results 文件。
