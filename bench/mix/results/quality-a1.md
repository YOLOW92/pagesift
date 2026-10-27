# 质量轮 A1（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift | 600 | 0.987 | 592 | 8 | 0 | 0 | 0 | 0 |
| crw | 600 | 0.995 | 597 | 3 | 0 | 0 | 0 | 0 |
| trafilatura | 600 | 0.987 | 592 | 8 | 0 | 0 | 0 | 0 |
| readability | 600 | 0.985 | 591 | 9 | 0 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| AEB | F1（AEB evaluate.py） | pagesift | 0.956 | 150 | 0.956 | 150 |
| AEB | F1（AEB evaluate.py） | crw | 0.846 | 150 | 0.846 | 150 |
| AEB | F1（AEB evaluate.py） | trafilatura | 0.950 | 150 | 0.950 | 150 |
| AEB | F1（AEB evaluate.py） | readability | 0.937 | 150 | 0.937 | 150 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | pagesift | 0.477 | 150 | 0.497 | 142 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | crw | 0.511 | 150 | 0.533 | 142 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | trafilatura | 0.392 | 150 | 0.407 | 142 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | readability | 0.403 | 150 | 0.426 | 142 |
| WCXB | 词级 F1（wcxb.py） | pagesift | 0.866 | 150 | 0.871 | 148 |
| WCXB | 词级 F1（wcxb.py） | crw | 0.815 | 150 | 0.825 | 148 |
| WCXB | 词级 F1（wcxb.py） | trafilatura | 0.839 | 150 | 0.844 | 148 |
| WCXB | 词级 F1（wcxb.py） | readability | 0.848 | 150 | 0.853 | 148 |
| WebMainBench | 词级 F1（webmain.py） | pagesift | 0.833 | 150 | 0.833 | 150 |
| WebMainBench | 词级 F1（webmain.py） | crw | 0.761 | 150 | 0.761 | 150 |
| WebMainBench | 词级 F1（webmain.py） | trafilatura | 0.833 | 150 | 0.833 | 150 |
| WebMainBench | 词级 F1（webmain.py） | readability | 0.840 | 150 | 0.840 | 150 |
