# 第 2 版参评工具：版本、安装与偏离默认的地方

核实日期 2026-10-28，机器同第 1 版（i7-10700 8 核 16 线程、15 GB、GTX 1070 8 GB）。第 1 版已有的工具按
[../tools.lock.md](../tools.lock.md) 和 `../locks/` 原样重装，版本不变；这里只写第 2 版新加的和这次的变化。

## pagesift

| 名称 | 来源 | 运行方式 |
|---|---|---|
| pagesift 0.80.9 | 提交 33ff97ab，`cargo build --release --locked -p pagesift`（发布工作流同一条命令） | `pagesift` / `pagesift-file` / `pagesift-crawl`：每份一个 CLI 进程（同第 1 版） |
| pagesift 0.80.9 serve | 同上 | `pagesift-serve*`：常驻 `pagesift serve --no-cache`，每份一次本机 HTTP 请求（`/v2/parse` 或 `/v2/scrape`） |
| pagesift 0.80.0 | GitHub 发布页 `pagesift-v0.80.0-x86_64-unknown-linux-gnu.tar.gz`（SHA-256 文件核对通过） | `pagesift-080*`：同第 1 版 |

## 新加的工具

| 领域 | 工具 | 版本 | 许可证 | 安装 | 和默认值不同的地方 |
|---|---|---|---|---|---|
| 网页正文 | Resiliparse | 1.1.0 | Apache-2.0 | `v2/install_py.sh resiliparse`（`locks/py/resiliparse.txt`） | `main_content=True`（正文提取开关；默认是整页转文本）。只出纯文本 |
| 网页正文 | newspaper4k | 0.9.6 | MIT | `newspaper4k[nlp,zh,ja,th,hi,bn,ta,np]` | `fetch_images=False`：默认 `parse()` 会联网下载页面图片挑头图，本基准所有工具只读快照、不联网。只装基础包时中、日、韩、阿拉伯文页面直接 ImportError，所以装语言扩展。只出纯文本 |
| 抓取 | Crawlee（Python） | 1.11.0 | Apache-2.0 | `locks/py/crawlee.txt` | `HttpCrawler`，内存存储；`HttpCrawler` 不解码，适配器按响应头 charset、否则 UTF-8 解码，所以它的乱码数不代表 Crawlee 本身 |
| PDF、Office | Kreuzberg | 4.10.4 | MIT | `locks/py/kreuzberg.txt` | `output_format="markdown"`；`use_cache=False`（默认开结果缓存，同一文件第二遍直接读缓存，测速就不准了） |
| PDF、Office | MinerU | 4.0.12 | MinerU Open Source License（基于 Apache-2.0） | `locks/py/mineru.txt`（默认安装：ONNX + llama.cpp）；`mineru-kit models download --tier standard` | 进程内调用 `mineru.parser.parse`（`mineru-kit parse` 的本地路径）。`mineru`：默认档 standard；`mineru-flash`：flash 档（官方“最快、不用模型”档）。Office/EPUB/CSV 官方只走 flash |
| Office | pandoc | 3.13 | GPL-2.0-or-later | GitHub 发布页 `pandoc-3.13-linux-amd64.tar.gz`（SHA-256 38aa33b3…） | `-t gfm`，按扩展名选读入格式 |

## 装了但跑不起来、照实记录的

- **Marker 2.0.0**（`marker-pdf`，Apache-2.0）：**强依赖较新的 NVIDIA GPU，默认安装在本机跑不起来**。Marker 2.0 不在自己进程里推理，
  而是另起推理服务：
  1. 默认安装的 torch（PyPI，CUDA 13 版）自动选 GPU，GTX 1070（sm_61）没有对应内核：`CUDA error: no kernel image is available`；
     `TORCH_DEVICE=cpu` 管不到它另起的服务进程。
  2. 把 GPU 隐藏（`CUDA_VISIBLE_DEVICES=`）后，它改用 Docker + `nvidia` 运行时起 vLLM 服务：`unknown or invalid runtime name: nvidia`。
  3. 它说明里的第三条路是另装 llama.cpp 官方发布的 `llama-server` 外部二进制；本基准不替参评工具装额外的外部程序（2026-10-28 用户决定），
     到此为止。

  所以 Marker 不出分，环境已删除。

## 资源设置（driver.py 的 per_worker_mb）

按实测单 worker 峰值内存预扣：MinerU flash 档 1.5 GB（B1 实测峰值 1.7 GB、B2 3.3 GB），MinerU 默认档 4 GB（实测 4.0 GB）。
MinerU 默认档的默认安装在本机用上了 GTX 1070（每个 worker 约 2 GB 显存，两个 worker 时 GPU 利用率 100%），和 RULES 第四节的预期一致。
另有整机内存看门狗 `v2/mem_guard.sh`：可用内存低于 1.5 GB 时杀掉占内存最多的评测 worker（这一份记超内存），日志
`~/mix-tools/mem_guard.log`。MinerU flash 档在 B1、B2 上曾按 800 MB 的低估开到 10 路，撑爆整机后被停下；那两批没再重跑，
已完成的份照实计分、没跑到的算没完成（RULES 第 1 版第四之二节）。

## 输入文件的变化

- B4：29 份输入原是指向 `tasks/` 的软链接，2026-10-28 清理磁盘时原文件被删。23 份从原始网址重新下载，SHA-256 和第 1 版一致；
  6 份 IDPF EPUB 从同一提交重新打包，内容相同、zip 时间戳不同，新旧哈希见 `samples/b4-repacked.tsv`。
- A4：15 份拦截页没有别的副本，第 2 版 A4 用剩下的 151 份（清单 `samples/a4-missing.tsv`）。
