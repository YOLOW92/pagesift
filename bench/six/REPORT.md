# 六项目第一轮实测与改进 — 2026-10-22

基于提交 `867c1a19e3204d9d50d4638d8287dd2aea9a20fe` 的本地修改；补丁为 candidate.patch (local `results/candidate.patch`)，SHA-256 `9998c042bcb79c6d88b72c4c77facee738f18d81796d429a9f5eef50b4b5eda1`。
这是第一轮受控诊断与针对性修复，**不是完整六产品排行榜，也不是最终发布验收**。
运行目录使用 UTC 时间，本文日期使用 Asia/Tokyo。源码版本见 [tools.lock.json](tools.lock.json)，环境见 [locks](locks/)。

## 已完成的两项产品改进

| 任务 | 改进前 | 改进后 | 证据范围 |
|---|---|---|---|
| 图片字段找到预期地址 | 7/12 | 12/12 | 公开的构造样例，覆盖占位图、懒加载和 srcset；不是全网准确率 |
| 短静态页训练后抓同站动态路径 | 只得到 Loading，漏掉金标 | 返回动态正文 | 同一受控页面序列；Crawlee 默认自适应模式也取得正文 |
| AEB 外部集正文回归 | F1 0.939262 | F1 0.939262 | 181/181 篇的 Markdown 字节完全一致 |

图片修复是 pagesift 自身字段回退逻辑的修复；未移植对手代码。渲染修复采用保守的 origin + 首级路径分组，浏览器内核兼容性仍按主机学习。它解决已复现的跨目录污染，**不保证同一目录内所有页面类型都能区分**，也未移植 Crawlee 的完整预测模型。

## 六个项目实际测到的内容

| 项目 | 实测 | 当前结论与边界 |
|---|---|---|
| Firecrawl v2.11.0 | 完整自托管 API 固定页面回放；Go 转换组件；官方 Python 4.30.1、JS 4.28.1 对接 pagesift | Python 9 项、JS 8 项全部通过，含 scrape/map/batch/crawl 轮询、取消与鉴权。完整服务与 pagesift 各 193 次有效回放全部上游 HTTP 200，181 篇 AEB 评分见下文；Go 组件独立结果不代表完整服务质量 |
| Crawlee 3.18.2 | Cheerio HTTP 抓取、系统 Chrome 的自适应混合页面测试 | 自适应测试取到动态金标。Cheerio 示例使用公开的常规 enqueueLinks handler，不把可自行配置的发现范围当成库的硬限制 |
| Scrapling 0.5 | 原生 parser 测试 140 项；6 个改版/负例 | 原生测试全过；默认重定位通过 5/6，其中相似干扰节点被误选。pagesift 精确选择器通过 4/6，缺少改版重定位；不据此宣称总体准确率 |
| Katana 1.8.0 | 本地已知 URL 图，HTTP + JS 发现 | 原生配置访问 6/7 个金标内容入口，pagesift 3/7；原生配置有 1 次 robots 违规；追加显式排除该构造禁用路径后仍为 6/7、违规 0。显式排除不是通用 robots 实现 |
| Essence v0.3.0 和 main 快照 | 未修改的 Markdown/图片模块，181 篇相同 AEB HTML | 两个版本此集 F1 均 0.766741，pagesift 为 0.939262。**仅离线组件**，排除网络、浏览器和服务质量校验，不能外推端到端胜率 |
| gallery-dl 1.32.17 | 真实 GenericExtractor 和 DownloadArchive | GenericExtractor 找到全部 12 个构造目标地址；归档重启后正确识别 20 个已下载条目，即使 URL 改变。pagesift 当前没有等价站点条目 ID 归档接口；不同于 URL 缓存或内容快照 |

发现图还显示响应头、iframe、HTMX 等是 pagesift 待补的发现来源。该轨道目前只做诊断，未增加产品抓取范围。

## 完整 Firecrawl 服务回放与逐页差距

使用锁定 API 镜像及 Playwright、PostgreSQL、Redis、RabbitMQ 五服务，串行回放同一份 181 篇 AEB HTML 与 12 个图片样例。Pagesift 使用 HTTP 引擎，Firecrawl 使用原生自动流水线；回放页面设置 CSP 禁止历史第三方资源，双方网络主机别名不同。该实验验证固定静态输入的端到端输出，不能代表实时互联网、云服务或动态页面表现。

| 原 AEB 评分器 | 正文 F1 | 精确率 | 召回率 |
|---|---:|---:|---:|
| Pagesift HTTP API | 0.939262 | 0.897281 | 0.985364 |
| Firecrawl 自托管 API | 0.804753 | 0.682737 | 0.979871 |

按 126 个站点聚类、配对 bootstrap 2000 次，F1 差值 Pagesift−Firecrawl 的 95% 百分位区间为 [0.106820, 0.166760]。此区间仅描述该冻结语料的重采样不确定性，不证明新站点总体优势。评分采用上游宏平均精确率/召回率的调和平均。

仍有 **14 篇 Pagesift 逐页 F1 落后**，完整列表与分数见 逐页差距 (local `results/firecrawl-full-scores/diff.json`)。人工查看前三项：nj.com 中嵌入推文正文保留不足；digg.com 中推荐内容残留较多；bgr.com 的取舍需要结合金标边界再定位，不能直接把更长输出当作更好正文。后续应使用独立页面家族验证规则，避免针对单站硬编码。

完整原始回放 (local `results/firecrawl-full-domain/summary.json`)、上游评分 (local `results/firecrawl-full-scores/eval/results.json`)、[镜像锁](locks/firecrawl-images.json) 均已保存。首次连接失败、域名回放 404 和私网 IP 校验失败已记录在 审计说明 (local `results/audit-notes.json`)，均排除出质量评分。完整栈与本机服务预算不同，**不发布速度或内存优劣排名**。

## 检查与资源

- 完整本地 CI (local `results/20261021T212113Z-full-local-ci-race-fixed-19e8bc/stdout.log`)：fmt、clippy、workspace 测试与文档测试共 536 项通过；Rust 1.88、cargo-deny、正文及爬取性能门槛通过。
- 首次 CI 暴露 Webhook 测试的 ACK/指标竞态：接收端已记录最后事件，但发送端 delivered 仍是 3。测试现有界等待确认后的指标，原有事件顺序、签名、重试数和 delivered=4 断言全部保留；失败日志未删除。
- 最终资源上限是共享 slice 的 8 个 CPU / 8 GiB RAM / 0 swap；早期准备上限更低，各次日志分别记录。单线程、探索性和 CI 数据不能混成最终计时排名。
- 清理旧缓存及重复快照，并在 CI 后删除本轮约 11 GiB 的 debug 产物；保留结果、输入、发行二进制和候选。本轮五个临时 Docker 镜像、容器与卷也已移除，原有 pagesift/debian 镜像保留，见 Docker 清理 (local `results/docker-cleanup.json`)。详见 初次清理 (local `results/cleanup-2026-10-22.json`) 与 CI 后清理 (local `results/post-ci-cleanup.json`)。

## 结果复核

- 结构化摘要 (local `results/summary.json`)、图片改前 (local `results/offline-baseline/fixture-scores.json`)、图片改后 (local `results/offline-candidate/fixture-scores.json`)。
- AEB 原评分器结果 (local `results/aeb-scores-with-main/eval/results.json`)；原始逐页输出位于 offline-baseline、offline-candidate 和 aeb-scores 目录。
- 自适应改前 (local `results/adaptive-baseline-ready/summary.json`)、自适应改后 (local `results/adaptive-candidate/summary.json`)、发现图 (local `results/discovery-scoped/summary.json`)。
- 改版诊断 v2 (local `results/relocation-baseline-v2/summary.json`)、归档语义 (local `results/gallery-archive-baseline/summary.json`)、Python SDK (local `results/sdk-candidate/python.json`)、JS SDK (local `results/sdk-candidate/js.json`)。

重定位 v1 把不同的文本节点分隔方式误记为选择器错误，已用双方一致的空白归一化修正，保留原记录并以 v2 为准。设置、编译和首次 CI 的失败尝试同样保留。

## 尚未完成的发布验收

更多真实站点、通用礼貌规则验证与独立渲染页面家族、站点专用增量工作负载、误匹配受控的 Rust 重定位，以及干净克隆和未见站点的最终发布复测，仍按 [总方案](../../docs/comparison-plan-2026-10-22.md) 继续。72 小时稳定性、全功能覆盖或六家全部指标领先均未在本轮得到证明。
