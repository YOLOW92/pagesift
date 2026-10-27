# 适配器

每个参评工具一个常驻 worker，统一协议，由 [`../driver.py`](../driver.py) 调度。工具本身的安装见
[`../tools.lock.md`](../tools.lock.md)。适配器是测试工具，用各工具自己的语言调用它；pagesift 本身不引入任何东西。

| 文件 | 工具 |
|---|---|
| `rust/`（独立 Cargo 工作区，`Cargo.lock` 锁版本） | crw、spider、pdf_oxide、extractous |
| `py_worker.py` | trafilatura、Scrapy、Crawl4AI、pymupdf4llm、Docling、MarkItDown、extractous（PyPI 轮子，备用） |
| `readability.mjs` | Mozilla Readability + Turndown（jsdom） |

## 协议（stdin / stdout，每行一个 JSON）

1. worker 启动、加载完工具后打一行：`{"ready": true, "tool": "...", "version": "...", "pid": 123}`。
   driver 把从启动到这一行的时间记成 `startup_ms`，不算进单份耗时。
2. driver 每发一行 `{"id": "...", "input": "<文件绝对路径或 URL>", "out": "<输出文件路径>", "url": "<可选：网页快照的原始地址>"}`，
   worker 转换后把 Markdown（或纯文本）写进 `out`，回一行
   `{"id": "...", "ok": true|false, "ms": <工具内转换耗时>, "bytes": <输出字节数>, "error": null|"..."}`。
3. stdin 关闭后 worker 退出。

`url` 只在输入是网页快照文件、清单里记了原始地址时才有；crw（`source_url`）、trafilatura（`url=`）、Readability
（jsdom 的 `url`）用它解析相对链接，其余工具忽略。

worker 只管转换。下面这些都在 driver 里，所有工具一样：

- **单份超时**：到点没回就把 worker 所在 cgroup 整组杀掉（`cgroup.kill`，连 Chromium 子进程一起），
  这一份记 `timeout`，换一个新 worker 接着跑剩下的。
- **内存上限与峰值**：每个 worker 用 `systemd-run --user --scope -p MemoryMax=… -p MemorySwapMax=0` 放进单独的
  cgroup；每份之前把 `memory.peak` 清零（内核 ≥ 6.12，按 fd 清零），之后读出来就是这一份的峰值，另外每 20 ms
  采一次 `memory.current` 兜底。被内核 OOM 杀掉的记 `oom`（看 `memory.events` 的 `oom_kill`）。
- **并发**：`--workers auto|N`；实际 worker 数 = min(N, 核数, 份数, (MemAvailable − 保留) / 该工具的预计内存,
  工具上限)。1 线程组用 `--workers 1`，16 线程组用 `--workers 16`。各工具内部的线程或进程用它自己的默认设置
  （见 tools.lock.md 每个工具的“内部并行”一栏）。
- **状态**：`ok`、`empty`（成功但输出为空）、`error`（工具报错）、`timeout`、`oom`、`crash`（worker 中途退出或起不来）。

抓取类工具（spider、Scrapy、Crawl4AI）的输入是 URL：driver 起 [`../replay.py`](../replay.py) 把快照文件放在
`http://127.0.0.1:<端口>/` 下回放，所有抓取工具读同一份快照，不联网。

## 冒烟测试

```bash
benchmarks/mix/smoke.sh /tmp/mix-smoke                 # 全部工具
benchmarks/mix/smoke.sh /tmp/mix-smoke crw scrapy      # 只跑几个
```

每个领域 3–4 份现有样本（网页快照 3 份、olmOCR PDF 3 份、Office 的 docx/pptx/xlsx/epub 各 1 份）。
轻量工具同时跑，Docling 系列随后逐个跑；整个脚本走 `tasks/0.80/bench.lock`。
