# 质量轮 B1（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pdf_oxide | 1269 | 0.954 | 1210 | 59 | 0 | 0 | 0 | 0 |
| pymupdf4llm | 1269 | 0.956 | 1213 | 56 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pdf_oxide | 0.389 | 1269 | 0.445 | 1208 |
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pymupdf4llm | 0.484 | 1269 | 0.548 | 1208 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pdf_oxide | 0.277 | 1269 | 0.335 | 1208 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pymupdf4llm | 0.390 | 1269 | 0.458 | 1208 |
