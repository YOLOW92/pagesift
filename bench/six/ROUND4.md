# 第四轮独立验收与四轮结果 — 2026-10-22

**结论：本次声明范围内的本地验收通过。** 四轮已完成基线、针对性 Rust 改进和独立样本复核。记录包含失败尝试、仍落后的样本与资源代价；本结论不等于六家全功能等价、第三方审计、整体性能领先或生产环境可靠性保证。

## 冻结版本与干净构建

- 基线 Git 提交：`a17f33d97383450535a4d79862fb850f88e5b8f2`。包含工作区原有 npm 发布修订；本任务没有改写或回退这些提交。
- 最终验收快照：`67f163ca34e449a26f0d3d9a82163d75622bd26e`，二进制 SHA-256：`45863e7e0628f5328abe9019593bb9426c508342994e7ed69841c21bb26858d9`。
- 快照提交只存在于 `bench/data/six/acceptance/source-v2` 临时克隆，原仓库改动没有由本任务提交、推送或发布。
- 从 `git clone --no-hardlinks` 得到独立源码，叠加明确的本地修改，在临时克隆提交后检查干净状态；目标目录起初为空，重新编译全部测试和应用。依赖下载缓存共享，已编译目标文件不复用。
- 最终构建清单 (local `results/round4/build-v2.json`)、干净构建日志 (local `results/20261021T235234Z-round4-refreeze-cold-build-d03527/stdout.log`)、源码归档 (local `results/round4/candidate-source.tar.gz`)、实际工具链 (local `results/round4/environment.json`)。最终还逐份核对源码哈希与原工作区，确认最终代码相同。

运行目录时间为 UTC，本文日期使用 Asia/Tokyo。两个临时克隆及所有失败输入保留；缓存清理不会删除源码、证据或命名二进制。

## 验收确实发现问题，并重新取样

首个快照 `379c30f48ac9530571a12421de0a62014c13a335` 的干净 CI 通过，但在预先冻结的四个新页面家族中只取到 **1/4** 的动态正文。相同首级路径或查询参数路由仍会受到相邻静态页面的学习结果污染。原始失败 (local `results/round4/render/summary.json`)。这批输入随后降为开发回归集，**没有继续冒充未见验收集**。

新增自研 Rust 守卫：当 HTTP 正文短、页面携带客户端脚本或事件处理器时，仍允许升级渲染；数据型 JSON 脚本不取消原有优化。[实现](../../crates/pagesift/src/engine.rs)、[学习边界说明](../../crates/pagesift/src/learn.rs)。修复后重新干净克隆和构建，生成另一主种子及输入，运行前冻结 最终协议 (local `results/round4/protocol-v2.json`)。

最终样本有 **7 类需客户端执行的页面 + 1 类已填充正文的 JSON 数据脚本控制页**，覆盖跨目录、同目录、query 路由、延迟、外部脚本、onload 和 module。每类先访问四篇短静态邻页。

| 模式 | 每次正确家族数 / 8 | 解释 |
|---|---|---|
| 修复前冻结候选 | 2、2、2、2、2 | 同输入、独立进程重复 |
| 最终候选 | 8、8、8、8、8 | 全部达到预置条件 |
| Crawlee 自适应模式，检测率 0.1 | 1, 6, 2, 1, 1 | 原生随机检测策略，保留波动 |
| Crawlee 原生 PlaywrightCrawler，显式等待 networkidle | 8/8 | 浏览器执行能力控制组，不是自适应配置 |

五次重复 (local `results/round4/render-repeats.json`)、最终逐页结果 (local `results/round4/render-final/summary.json`)、浏览器控制组 (local `results/round4/browser-control/summary.json`)、已降级样本的修复复测 (local `results/round4/render-regression/summary.json`)。五次重复不是 40 个独立站点；也不能把自适应漏判写成 Crawlee 不会渲染这些页面。

### 资源代价也有实测

8 篇仅带统计脚本的短静态页，浏览器调用从 **3 次增加到 8 次**；纯 JSON 数据脚本仍是 **3 → 3**。这是减少漏正文所付出的保守代价，不是免费优化。成本诊断 (local `results/round4/render-cost/summary.json`)。真实站点上的无收益渲染比例与长期成本尚未估计；没有发布渲染速度或内存优劣榜。

## 六家本轮实际复核范围

### 内容与图片

新图片集为 **4 种公开构造模板 × 6 个随机 URL 实例**，目标是识别明确的图片候选地址，不是 24 个独立真实站点。实际输入覆盖空白 src、blob 占位、懒加载 srcset 和普通 src。

| 实现 | images 字段命中目标 / 24 | Markdown 出现目标 URL / 24 | 边界 |
|---|---:|---:|---|
| Pagesift 原改进前二进制 | 6 | 18 | 完整 HTTP API |
| 最终 Pagesift | 24 | 24 | 完整 HTTP API |
| Firecrawl v2.11.0 自托管 | 12 | 6 | 五服务全部启动，原生自动流水线 |
| gallery-dl 1.32.17 | 24 | 不适用 | 真实 GenericExtractor，不下载媒体 |
| Essence v0.3.0 / main | 不适用 | 12 / 12 | 未修改的离线 Markdown 模块 |

Scrapling 的原始 `img::attr(src)` 选择器只在 6/24 包含目标，但它没有被包装成智能图片提取器，因此不参加图片能力排名。

图片逐例结果 (local `results/round4/images-final/summary.json`)、完整服务评分 (local `results/round4/firecrawl-image-scores.json`)。Markdown 列只检查目标 URL 出现，不等于所有 Markdown 结构都正确，也不把其他尺寸的有效图片自动判错。

从已冻结 Tranco 排名抽取 12 个新主种子域名，不补抽失败项：6 份 HTML 成功、1 个非 HTML、2 个 robots HTTP 错误、3 个 robots 不可用。全部抓取记录 (local `results/round4/live-fetch-v2.json`)。排除范围仅是旧 `bench/data/inputs/manifest.jsonl` 的域名，不能保证与每个历史数据集都没有域名交集。

这 6 份新快照与 24 个图片实例做固定回放，Pagesift 与 Firecrawl 各有 **30 个有效实验响应，另加 1 个不计分的就绪探针**；均核验 API 成功及上游 HTTP 200。完整服务原始结果 (local `results/round4/firecrawl-final/summary.json`)。快照按 HTTP 声明编码统一 UTF-8，原字节哈希、替换字符数和转换信息均保留。回放 CSP 禁止历史第三方脚本/框架/资源；这不是实时网页的动态渲染质量验证。6 份真实快照没有人工正文金标，只用于新输入端到端冒烟，不产生 F1 胜负。

### 发现、结构化与增量

| 轨道 | 实际结果 | 可得结论与限制 |
|---|---|---|
| 新导航图 8 个入口 | Pagesift 原版本 3/8，最终 8/8；robots 和写请求均为 0 | 包含 frame、HTMX/data-hx-get、HTML next、复杂 Link 响应头 |
| 同图 Crawlee Cheerio handler | 2/8 | 只使用常规 enqueueLinks，不能当成库的发现上限 |
| Katana 1.8.0 | 原生 6/8，访问禁用路径一次并发出一次写请求；显式排除构造禁用/写入口后 6/8、两者均 0 | 原生安全端点发现与只读 RAG 抓取目的不同；显式排除不代表通用 robots 实现 |
| 12 个字段改版/负例 | Pagesift 精确选择器正例 3/6、负例拒绝 6/6；Scrapling 默认重定位正例 6/6、负例拒绝 3/6 | 同为 9/12，但错误代价不同。Rust 改版重定位仍未实现 |
| 原生 Scrapling parser 回归 | 140 项全部通过 | 独立于字段小样本校准 |
| gallery-dl 与 Rust 条目存储 | URL 全部变化、重开后双方 31/31 识别已完成 ID；Rust 另有 7/7 显式版本持久化 | 是真实归档/存储接口，不是站点 extractor 或自动增量爬取集成 |
| 真实 SIGKILL 恢复 | Pagesift 改前、改后及 Crawlee 均 97/97 唯一交付，已保存 URL 重启后重抓 0 | 在途请求允许重试，非网络 exactly-once 或断电保证 |

导航图 (local `results/round4/discovery-final/summary.json`)、业务身份明确的结构化样例 (local `results/round4/structured-final/summary.json`)、条目接口 (local `results/round4/items-final/summary.json`)、进程恢复 (local `results/round4/process-recovery-final/summary.json`)。第一轮只在脚本字符串里出现的 `/script-route` 仍没有补齐，不能将新的 8/8 当成旧图 7/7。

Crawlee 的进程恢复等待其原生队列锁释放，没有手动清锁或增加自定义去重；修复前 Pagesift 本就能通过普通 SIGKILL 恢复。本轮保留第三轮确定性“结果已落盘、done 未落盘”回归矩阵，收益来自该边界修复，不归功于原本已有的恢复能力。

## 旧领域全量回归与性能门槛

| 领域 | 最终复测规模 | 结果 |
|---|---|---|
| AEB 文章正文 | 181 篇，原评分器 | F1 **0.939262**，精确率 0.897281，召回率 0.985364 |
| 已冻结 HTTP API | 181 篇 AEB + 12 个图片样例 | 正文及图片 193/193 一致，links 变化 0 |
| 新随机 Markdown 往返 | 3 个新种子 × 200 篇 | 原版本和候选 600/600 字节一致，各组显示结构 F1 1.000；Essence 组件均值显示 0.825–0.835 |
| 日期 | 1000 页，两种 URL 模式 | 两种模式分别 1000/1000 预测一致，使用原 htmldate 金标评分 |
| RAG | Chroma 472 + Qasper 1279 条问题 | queryFilter 区间和 recursive chunk 区间全部一致，原评分器复跑 |
| PDF | 五组、1269 份文件 | 1269/1269 Markdown 字节一致；历史五组口径平均 **71.02%** |
| Rust 本地 CI | 542 项测试及文档测试 | fmt、clippy、Rust 1.88、cargo-deny 和性能门槛通过 |
| 官方 SDK | Python 9 + JS 8 | 共 17 项通过 |
| 性能门槛重复 | 正文、CSS/XPath/RAG、爬取三个实际二进制，串行 5 次 | 全部通过，仅验证门槛，不作跨工具速度排名 |

各领域摘要 (local `results/round4/domains-final/summary.json`)、最终 AEB 分数 (local `results/round4/aeb-final/eval/results.json`)、PDF 历史口径 (local `results/round4/domains-final/pdf/historical-aggregation.json`)、五次门槛及二进制哈希 (local `results/round4/perf-verified/summary.json`)。PDF 仍存在公式、表格及无文本层页面缺口；71.02% 是回归持平，不是本轮 PDF 改进。PDF 一次合并运行得到的 60.1% 使用不同的 baseline 汇总权重，诊断日志保留，不与历史五组等权均值混表。

这些旧领域是回归数据，不称为全新独立测试。AEB 仍有第一轮记录的 **14 篇落后文章**，正文没有针对这些样本改写规则。此前 Firecrawl 固定 AEB 参考 F1 0.804753 和 Essence 离线参考仍在旧报告中；本轮不把旧结果伪装成全新实时测试。

### 旧多站点陷阱回归没有隐藏下降

原 wild 源站使用新主种子、冻结 AEB 正文载荷，40 个模拟站点 × 60 内容页，包含重复/会话/日历陷阱、robots、503 和 429。每个实现分别重启源站，源站自行计数；只管理本任务 PID，没有使用旧 shell 的全局 pkill。

| 配置 | 原版本覆盖率 | 最终候选覆盖率 | 变化 | 候选 robots 违规 |
|---|---:|---:|---:|---:|
| fast | 73.50% | 72.50% | -1.00 个百分点 | 0 |
| polite | 72.42% | 72.29% | -0.125 个百分点 | 0 |

均在运行前记录的 **最多下降 2 个百分点**容差内；这是带时间窗口限流的单次质量回归，不是证明新增发现能力在复杂网络上必然提高覆盖率。预置条件 (local `results/round4/wild-final/protocol.json`)、完整源站结果 (local `results/round4/wild-final/summary.json`)。没有把下降写成提升，也没有据此发布微小速度差排名。

## 资源、失败审计与交付

- 原生工具共享 8 个逻辑 CPU（0–7）、8 GiB RAM、0 swap 的 slice；两次冷构建均记录峰值约 4.1 GiB。机器后台桌面应用保持运行。
- Docker 子进程单独受容器限制：五服务上限合计 6784 MiB / 7.2 CPU，驱动上限 1024 MiB / 0.5 CPU；与其他重负载验收串行运行。全部服务的资源采样保留，未只计算 API。
- 本轮测试容器、卷和五个镜像已清理，原有镜像保留；仅清理闲置的构建缓存，保留命名二进制、两份哈希不同的同名性能示例和全部数据/日志。清理记录 (local `results/round4/cleanup.json`)。磁盘清理后可用约 **30.2 GiB**。
- 首个旧领域脚本把参考目录当文件读，修正后在新输出目录完整重跑；真实快照中的 Unicode 行分隔符导致初次 ad-hoc JSONL 读取错误，改为按物理换行读取，原始响应均完整；同名 `perf_gate` 示例互相覆盖，最终依赖文件定位实际哈希二进制并重跑五次。失败尝试与诊断都保留，没有当成产品质量失败或最终成功。

机器可读总摘要 (local `results/round4/summary.json`)、校验清单 (local `results/round4/manifest.json`)、验收脚本包 (local `results/round4/comparison-kit.tar.gz`)。输入、评分器、版本锁和原始输出均保留；原仓库没有由本任务提交、推送或发布。

## 尚未兑现的能力边界

1. AEB 的 14 篇落后文章；应使用跨站引用块/推荐块金标研究清理规则，不能在该集反复调分仍称独立验收。
2. Rust 自适应重定位；需要拒绝/歧义策略和明确业务身份，当前只有精确选择器。
3. 自动站点条目识别、真实分页 API 断点、删除同步和站点专用适配；目前交付显式 Rust Store::items 接口。
4. 真实站点的无收益渲染成本、更多浏览器内核/站点家族、长期稳定性及配置覆盖；72 小时稳定性未运行。

以上不计作已完成或总体领先。本地验收支持的是本次具体修改及已声明任务的证据链。
