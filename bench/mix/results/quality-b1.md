# 质量轮 B1（竞品；pagesift 一栏待最终二进制）

olmOCR 主分是官方 benchmark.py 一次运行的汇总（Average Score：各类通过率的平均，含它给每个 PDF 加的 baseline 类），所有工具用同一个评分脚本；另附“去掉 baseline 的各类平均”。docs/benchmarks.md 旧表的 64.4 是按子集分别跑、分别汇总的，口径不同，不能和这里直接比。

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift-file | 1269 | 0.950 | 1205 | 64 | 0 | 0 | 0 | 0 |
| pdf_oxide | 1269 | 0.954 | 1210 | 59 | 0 | 0 | 0 | 0 |
| pymupdf4llm | 1269 | 0.956 | 1213 | 56 | 0 | 0 | 0 | 0 |
| docling | 144 | 0.993 | 143 | 0 | 0 | 1 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.626 | 1269 | 0.709 | 1203 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.389 | 1269 | 0.446 | 1203 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.484 | 1269 | 0.549 | 1203 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.561 | 1269 | 0.651 | 1203 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.277 | 1269 | 0.336 | 1203 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.390 | 1269 | 0.459 | 1203 |

## 分数（模型子集 10%（含 Docling））

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.650 | 144 | 0.708 | 137 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.422 | 144 | 0.470 | 137 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.479 | 144 | 0.530 | 137 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | docling | 0.696 | 144 | 0.702 | 137 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.587 | 144 | 0.650 | 137 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.314 | 144 | 0.364 | 137 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.383 | 144 | 0.437 | 137 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | docling | 0.637 | 144 | 0.642 | 137 |
