# 质量轮 B1（竞品；pagesift 一栏待最终二进制）

olmOCR 主分是官方 benchmark.py 一次运行的汇总（Average Score：各类通过率的平均，含它给每个 PDF 加的 baseline 类），所有工具用同一个评分脚本；另附“去掉 baseline 的各类平均”。docs/benchmarks.md 旧表的 64.4 是按子集分别跑、分别汇总的，口径不同，不能和这里直接比。

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift-100-file | 1269 | 0.999 | 1268 | 1 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| olmOCR | olmOCR-bench 官方总分（各类平均，含 baseline 类） | pagesift-100-file | 0.645 | 1269 | 0.645 | 1268 |
| olmOCR 去掉 baseline | 各类平均，不含 baseline 类 | pagesift-100-file | 0.574 | 1269 | 0.575 | 1268 |
