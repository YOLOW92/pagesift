# 质量轮 B4（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| extractous | 81 | 0.679 | 55 | 0 | 26 | 0 | 0 | 0 |
| markitdown | 81 | 1.000 | 81 | 0 | 0 | 0 | 0 | 0 |
| docling | 7 | 1.000 | 7 | 0 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| B4 dev | 结构保留率（score.py，开发集） | extractous | 0.101 | 40 | 0.184 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | markitdown | 0.833 | 40 | 0.915 | 28 |
| B4 holdout | 结构保留率（score.py，留出集） | extractous | 0.075 | 36 | 0.147 | 23 |
| B4 holdout | 结构保留率（score.py，留出集） | markitdown | 0.717 | 36 | 0.687 | 23 |

## 分数（模型子集 10%（含 Docling））

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| B4 dev | 结构保留率（score.py，开发集） | extractous | 0.051 | 5 | 0.120 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | markitdown | 0.847 | 5 | 1.000 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | docling | 0.695 | 5 | 0.693 | 4 |
| B4 holdout | 结构保留率（score.py，留出集） | extractous | 0.000 | 2 | 0.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | markitdown | 1.000 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | docling | 0.889 | 2 | 1.000 | 1 |
