# 质量轮 B1（竞品；pagesift 一栏待最终二进制）

olmOCR 主分是官方 benchmark.py 一次运行的汇总（Average Score：各类通过率的平均，含它给每个 PDF 加的 baseline 类），所有工具用同一个评分脚本；另附“去掉 baseline 的各类平均”。docs/benchmarks.md 旧表的 64.4 是按子集分别跑、分别汇总的，口径不同，不能和这里直接比。

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift-file | 1269 | 0.951 | 1207 | 62 | 0 | 0 | 0 | 0 |
| pagesift-080-file | 1269 | 0.950 | 1205 | 64 | 0 | 0 | 0 | 0 |
| pdf_oxide | 1269 | 0.954 | 1210 | 59 | 0 | 0 | 0 | 0 |
| pymupdf4llm | 1269 | 0.956 | 1213 | 56 | 0 | 0 | 0 | 0 |
| kreuzberg | 1269 | 0.954 | 1210 | 59 | 0 | 0 | 0 | 0 |
| mineru-flash | 1269 | 0.472 | 599 | 1 | 0 | 0 | 0 | 2 |
| mineru | 1269 | 0.113 | 144 | 0 | 0 | 0 | 0 | 0 |
| docling | 144 | 1.000 | 144 | 0 | 0 | 0 | 0 | 0 |
| docling-gpu | 144 | 0.986 | 142 | 0 | 2 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.628 | 1269 | 0.707 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-080-file | 0.626 | 1269 | 0.707 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.389 | 1269 | 0.469 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.484 | 1269 | 0.444 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | kreuzberg | 0.417 | 1269 | 0.531 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | mineru-flash | 0.102 | 1269 | 0.494 | 66 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | mineru | 0.098 | 1269 | 0.906 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.563 | 1269 | 0.560 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-080-file | 0.561 | 1269 | 0.560 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.277 | 1269 | 0.204 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.390 | 1269 | 0.167 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | kreuzberg | 0.310 | 1269 | 0.296 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | mineru-flash | 0.028 | 1269 | 0.241 | 66 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | mineru | 0.095 | 1269 | 0.874 | 66 |

## 分数（模型子集 10%（含 Docling））

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.650 | 144 | 0.707 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-080-file | 0.650 | 144 | 0.707 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.422 | 144 | 0.469 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.479 | 144 | 0.444 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | kreuzberg | 0.419 | 144 | 0.531 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | mineru-flash | 0.100 | 144 | 0.494 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | mineru | 0.875 | 144 | 0.904 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | docling | 0.697 | 144 | 0.593 | 64 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | docling-gpu | 0.694 | 144 | 0.593 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.587 | 144 | 0.560 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-080-file | 0.587 | 144 | 0.560 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.314 | 144 | 0.204 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.383 | 144 | 0.167 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | kreuzberg | 0.310 | 144 | 0.296 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | mineru-flash | 0.028 | 144 | 0.241 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | mineru | 0.853 | 144 | 0.872 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | docling | 0.637 | 144 | 0.389 | 64 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | docling-gpu | 0.637 | 144 | 0.389 | 64 |
