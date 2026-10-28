# 第 2 版运行记录：和冻结规则不一致的地方（2026-10-28）

规则（`RULES.md`）冻结后不改；运行中的偏离、事故和用户的决定都记在这里，报告引用本文件。

## 用户的决定

- **C3 长时间运行去掉**（第 3 阶段只做 B5）。
- **MinerU flash 档在 B1、B2 上不重跑**：按低估的内存预扣开到 10 路时撑爆整机、被停下；已完成的份照实计分，
  没跑到的算没完成（第 1 版 RULES 第四之二节）。用户原话：这是它的问题、我们的优势。
- **Marker 不出分**：默认安装在本机跑不起来（详见 `tools.lock.md`），不替它另装外部的 `llama-server`。
- 本轮所有测速都基于 pagesift 0.80.9；其间 main 上出了 0.80.10，不测。

## 输入

- A4 缺 15 份（`samples/a4-missing.tsv`），B4 有 6 份重新打包（`samples/b4-repacked.tsv`），见 `tools.lock.md`。

## 评测框架的修正（只改 `benchmarks/mix/`，pagesift 本身不改）

1. **模型工具的单 worker 内存上限**：RULES 第五节写 8 GB，`run_batch.py` 第一次仍用第 1 版的 4 GB。MinerU 默认档
   B2 有 2 份、B3 有 2 份被这个错误的上限杀掉；改成 8 GB 后 B2、B3 整批重跑（B1 没有被杀的份，不重跑）。
2. **打分时“只跑 10% 子集”的工具**原来只认 `docling*`，MinerU 默认档的完成率按全集算错了；改成 `docling*`、`marker*`、`mineru`。
3. **准入排队拖住 driver**：一个作业的份数领完后，还在排队等内存的线程不退出，driver 收不了尾。`admit.acquire` 加了取消
   条件。Docling（CPU）B3 那一批 8 份已全部跑完，driver 被手动结束，它的 `summary.json` 为空，逐份结果完整。
4. **16 路测速的内存预扣**：第一次 16 路测速按 `driver.py` 里手写的预估预扣，pagesift 的预估（600 MB）是实测峰值（40 MB）
   的 15 倍，准入每次放不满就等 1 秒，三个 pagesift 的吞吐都被卡在约“份数 / 1 秒”（B1 142.3、B4 80）。PDF、Office 的 16 路
   对所有工具按同一条规则重测：预扣 = 1 路实测峰值 × 1.5（最少 100 MB），表由 `peaks.py` 生成。第 1 版报告“pagesift
   16 路扩展差（142 对 526）”很可能也是这个原因。
5. **测速撞车**：16 路网页正文测速期间曾有约 1 分钟在拉 Firecrawl 镜像（解压占 CPU），Readability、Resiliparse、newspaper4k
   的 16 路随后单独重测。
6. **newspaper4k**：第一次只装了基础包（中日韩阿拉伯文页面报 ImportError），且默认 `parse()` 会联网下载图片（34 份等网络超时）；
   装语言扩展、`fetch_images=False` 后 A1、A2、A4 整批重跑，用重跑的结果。

## 已知的测量局限

- 抓取（A2 回放）只有一个主机 `127.0.0.1`：按主机限速的工具（pagesift serve、Crawlee）在这里吃亏，单份中位耗时约
  1 秒和 0.5 秒都来自同主机请求间隔，不是抓取本身。

## 为省时间砍掉的测速项（用户决定，2026-10-28 21:30）

- **4 路不测**：本机测速只测单路和 16 路。A2 第一次误按第 1 版脚本多跑了 4 路，结果已全部删除，不进报告。
- **MinerU 默认档的 16 路不测**：它单 worker 峰值约 4–5 GB，按内存最多只能开 1–2 路，16 路和 1 路几乎是同一个数，却要多跑十几分钟。

## 第 3 阶段（21:45 以后）

- **Firecrawl 自托管部署失败**：官方镜像照 compose 配置起 5 个服务（`firecrawl_up.sh`），RabbitMQ 容器读不了
  `/var/lib/rabbitmq/.erlang.cookie`（`eacces`），挂命名卷后仍一样；API 依赖 RabbitMQ 一起退出。只试了两次，没有再深挖。
  A3 只比 pagesift 和 Crawl4AI。
- **B5 的 Docling CPU 版中途停掉**：整机内存紧（看门狗 21:43 杀过一个 5 GB 的 MinerU worker），而 Docling GPU 版 B5 已跑完，
  B1 上 CPU、GPU 两版质量分相同。B5 只报 Docling GPU 版。
- **A3 与 B5 的 MinerU 同时在跑**：A3 的耗时主要是网络和渲染，报告注明。
