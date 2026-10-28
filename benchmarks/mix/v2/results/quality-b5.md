# 质量轮 B5（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift-file | 134 | 0.112 | 15 | 119 | 0 | 0 | 0 | 0 |
| pdf_oxide | 134 | 0.112 | 15 | 119 | 0 | 0 | 0 | 0 |
| pymupdf4llm | 134 | 0.112 | 15 | 119 | 0 | 0 | 0 | 0 |
| kreuzberg | 134 | 0.112 | 15 | 119 | 0 | 0 | 0 | 0 |
| docling-gpu | 134 | 1.000 | 134 | 0 | 0 | 0 | 0 | 0 |
| mineru | 134 | 0.157 | 21 | 0 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.037 | 134 | 0.500 | 15 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.037 | 134 | 0.500 | 15 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.037 | 134 | 0.500 | 15 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | kreuzberg | 0.037 | 134 | 0.500 | 15 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.000 | 134 | 0.000 | 15 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.000 | 134 | 0.000 | 15 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.000 | 134 | 0.000 | 15 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | kreuzberg | 0.000 | 134 | 0.000 | 15 |

## 分数（模型子集 10%（含 Docling））

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-file | 0.037 | 134 | — | 0 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.037 | 134 | — | 0 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.037 | 134 | — | 0 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | kreuzberg | 0.037 | 134 | — | 0 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | docling-gpu | 0.410 | 134 | — | 0 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | mineru | 0.081 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-file | 0.000 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.000 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.000 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | kreuzberg | 0.000 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | docling-gpu | 0.125 | 134 | — | 0 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | mineru | 0.047 | 134 | — | 0 |
