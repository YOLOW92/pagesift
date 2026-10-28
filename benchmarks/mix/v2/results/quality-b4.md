# 质量轮 B4（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift-file | 81 | 1.000 | 81 | 0 | 0 | 0 | 0 | 0 |
| pagesift-080-file | 81 | 1.000 | 81 | 0 | 0 | 0 | 0 | 0 |
| extractous | 81 | 0.679 | 55 | 0 | 26 | 0 | 0 | 0 |
| markitdown | 81 | 1.000 | 81 | 0 | 0 | 0 | 0 | 0 |
| kreuzberg | 81 | 0.975 | 79 | 1 | 0 | 0 | 0 | 1 |
| pandoc | 81 | 1.000 | 81 | 0 | 0 | 0 | 0 | 0 |
| mineru-flash | 81 | 0.975 | 79 | 2 | 0 | 0 | 0 | 0 |
| docling | 7 | 1.000 | 7 | 0 | 0 | 0 | 0 | 0 |
| docling-gpu | 7 | 1.000 | 7 | 0 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| B4 dev | 结构保留率（score.py，开发集） | pagesift-file | 0.987 | 40 | 0.978 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | pagesift-080-file | 0.987 | 40 | 0.978 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | extractous | 0.101 | 40 | 0.184 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | markitdown | 0.833 | 40 | 0.915 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | kreuzberg | 0.886 | 40 | 0.918 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | pandoc | 0.667 | 40 | 0.732 | 28 |
| B4 dev | 结构保留率（score.py，开发集） | mineru-flash | 0.733 | 40 | 0.883 | 28 |
| B4 holdout | 结构保留率（score.py，留出集） | pagesift-file | 0.988 | 36 | 0.976 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | pagesift-080-file | 0.988 | 36 | 0.976 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | extractous | 0.075 | 36 | 0.141 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | markitdown | 0.717 | 36 | 0.664 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | kreuzberg | 0.816 | 36 | 0.931 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | pandoc | 0.705 | 36 | 0.647 | 22 |
| B4 holdout | 结构保留率（score.py，留出集） | mineru-flash | 0.704 | 36 | 0.788 | 22 |

## 分数（模型子集 10%（含 Docling））

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| B4 dev | 结构保留率（score.py，开发集） | pagesift-file | 0.994 | 5 | 0.987 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | pagesift-080-file | 0.994 | 5 | 0.987 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | extractous | 0.051 | 5 | 0.120 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | markitdown | 0.847 | 5 | 1.000 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | kreuzberg | 0.915 | 5 | 0.933 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | pandoc | 0.576 | 5 | 0.573 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | mineru-flash | 0.718 | 5 | 0.933 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | docling | 0.695 | 5 | 0.693 | 4 |
| B4 dev | 结构保留率（score.py，开发集） | docling-gpu | 0.695 | 5 | 0.693 | 4 |
| B4 holdout | 结构保留率（score.py，留出集） | pagesift-file | 1.000 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | pagesift-080-file | 1.000 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | extractous | 0.000 | 2 | 0.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | markitdown | 1.000 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | kreuzberg | 0.889 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | pandoc | 0.611 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | mineru-flash | 0.500 | 2 | 0.600 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | docling | 0.889 | 2 | 1.000 | 1 |
| B4 holdout | 结构保留率（score.py，留出集） | docling-gpu | 0.889 | 2 | 1.000 | 1 |
