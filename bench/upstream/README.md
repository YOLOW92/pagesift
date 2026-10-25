# 基准上游评分器（vendored）

这里只保留**评分本身需要的最小代码**，不含上游的样例数据、`results/`、`.git` 和虚拟环境。
完整仓库请按下表的 URL 与 commit 自行 clone；本目录代码与这些 commit 一致。

| 目录 | 上游 | Commit | License | 用途 |
| :--- | :--- | :--- | :--- | :--- |
| `MinerU-HTML/` | https://github.com/opendatalab/MinerU-HTML | `73cf266690befd209cae7e6fdff9716d5b31a976` | 见 `LICENSE` | WebMainBench 全量 ROUGE-5（`eval_baselines.utils.calc_rouge_n_score`） |
| `WebMainBench/` | https://github.com/opendatalab/WebMainBench | `9d991bdc00c57b57521499494d96be85c31317ba` | 见 `LICENSE` | 545 页五维编辑距离 + TEDS（`webmainbench.metrics.calculator.MetricCalculator`） |

## 为什么 vendored

评分器版本必须与报告里记录的 commit 一致，否则同一份预测会算出不同分数。
把评分代码随仓库固定下来，可以让 `bench/p4_2_webmainbench/score_full.py` 与
`score_545_offline.py` 直接可跑，不需要联网 clone。

## 只保留了哪些路径

- `MinerU-HTML/eval_baselines/`：`score_full.py` 通过 `sys.path.insert(mineru_dir)` 导入它。
- `WebMainBench/webmainbench/`：`score_545_offline.py` 构造同名包后导入 `metrics.calculator`。

其余目录（`benchmark/`、`results/`、`data/`、`mineru_html/`、`requirements/`）与评分无关，已移除。

## 运行依赖

```bash
pip install rouge_score jieba rapidfuzz levenshtein numpy bs4 lxml jsonlines apted openai python-dotenv nltk fuzzysearch pypdf tqdm
python -c "import nltk; nltk.download('punkt_tab')"
```

## 其他用到的上游（未 vendored，按需 clone）

WCEB、olmOCR-bench、Crawl Maze、robotstxt-spec-test 只在跑对应基准时需要，
体积大且不进评分路径，因此不入库；复现命令见 `docs/benchmarks/` 下各报告。
