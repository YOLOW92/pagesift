# 质量轮 A1（竞品；pagesift 一栏待最终二进制）

| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |
|---|---|---|---|---|---|---|---|---|
| pagesift | 600 | 0.987 | 592 | 8 | 0 | 0 | 0 | 0 |
| pagesift-080 | 600 | 0.987 | 592 | 8 | 0 | 0 | 0 | 0 |
| crw | 600 | 0.995 | 597 | 3 | 0 | 0 | 0 | 0 |
| trafilatura | 600 | 0.987 | 592 | 8 | 0 | 0 | 0 | 0 |
| readability | 600 | 0.985 | 591 | 9 | 0 | 0 | 0 | 0 |
| resiliparse | 600 | 0.977 | 586 | 14 | 0 | 0 | 0 | 0 |
| newspaper4k | 600 | 0.982 | 589 | 7 | 4 | 0 | 0 | 0 |

## 分数（全部样本）

| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |
|---|---|---|---|---|---|---|
| AEB | F1（AEB evaluate.py） | pagesift | 0.956 | 150 | 0.956 | 148 |
| AEB | F1（AEB evaluate.py） | pagesift-080 | 0.956 | 150 | 0.956 | 148 |
| AEB | F1（AEB evaluate.py） | crw | 0.846 | 150 | 0.845 | 148 |
| AEB | F1（AEB evaluate.py） | trafilatura | 0.950 | 150 | 0.950 | 148 |
| AEB | F1（AEB evaluate.py） | readability | 0.937 | 150 | 0.937 | 148 |
| AEB | F1（AEB evaluate.py） | resiliparse | 0.874 | 150 | 0.872 | 148 |
| AEB | F1（AEB evaluate.py） | newspaper4k | 0.928 | 150 | 0.934 | 148 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | pagesift | 0.480 | 150 | 0.502 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | pagesift-080 | 0.477 | 150 | 0.498 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | crw | 0.511 | 150 | 0.527 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | trafilatura | 0.392 | 150 | 0.409 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | readability | 0.403 | 150 | 0.426 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | resiliparse | 0.453 | 150 | 0.485 | 139 |
| Firecrawl | 正文召回 truth_recall（pagesift bench） | newspaper4k | 0.275 | 150 | 0.289 | 139 |
| WCXB | 词级 F1（wcxb.py） | pagesift | 0.866 | 150 | 0.883 | 145 |
| WCXB | 词级 F1（wcxb.py） | pagesift-080 | 0.866 | 150 | 0.883 | 145 |
| WCXB | 词级 F1（wcxb.py） | crw | 0.815 | 150 | 0.837 | 145 |
| WCXB | 词级 F1（wcxb.py） | trafilatura | 0.839 | 150 | 0.859 | 145 |
| WCXB | 词级 F1（wcxb.py） | readability | 0.848 | 150 | 0.868 | 145 |
| WCXB | 词级 F1（wcxb.py） | resiliparse | 0.777 | 150 | 0.790 | 145 |
| WCXB | 词级 F1（wcxb.py） | newspaper4k | 0.738 | 150 | 0.762 | 145 |
| WebMainBench | 词级 F1（webmain.py） | pagesift | 0.833 | 150 | 0.831 | 145 |
| WebMainBench | 词级 F1（webmain.py） | pagesift-080 | 0.833 | 150 | 0.831 | 145 |
| WebMainBench | 词级 F1（webmain.py） | crw | 0.761 | 150 | 0.766 | 145 |
| WebMainBench | 词级 F1（webmain.py） | trafilatura | 0.833 | 150 | 0.836 | 145 |
| WebMainBench | 词级 F1（webmain.py） | readability | 0.840 | 150 | 0.846 | 145 |
| WebMainBench | 词级 F1（webmain.py） | resiliparse | 0.796 | 150 | 0.803 | 145 |
| WebMainBench | 词级 F1（webmain.py） | newspaper4k | 0.706 | 150 | 0.723 | 145 |
