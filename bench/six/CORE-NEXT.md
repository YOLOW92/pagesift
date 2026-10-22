# 自研核心迭代：文章归属、身份重定位与紧凑断点

本轮直接修改 Rust 产品核心，未导入第三方正文或重定位实现。文章归属约束、跨标签业务身份、内容摘要断点均已验证。正文在新的保留样本上改善，并超过固定 Essence 正文组件；Digg 单页超过固定 Firecrawl 参考。**仍有 11 篇原先落后文章未追平，不宣称六个项目的全能力总体超越。**

## 正文核心：从“更大”转向“属于本篇”

- 标题查找限制在所属 article，避免推荐列表里的 H1 把 H2 正文带到整页。
- 将文档 title 与 article 内标题关联：规范化直接包含，或有效词集合覆盖至少 80%；多个 article 同时对应时放弃此优先级。
- 关联明确时保留完整 article，不继续按 85% 子块比例下钻，防止漏掉结尾短章节。
- 文档级标题位于 article 外时，单独保留标题，不将周边推荐一起引入；支持中文等无空格分词标题。

代码没有站点名、URL、文章 ID 或金标片段分支。[设计与边界](../../docs/editorial-core.md)、[五个结构回归测试（含标题前后顺序两个情形）](../../crates/pagesift-content/tests/editorial_regions.rs)。这些是正文选择证据，并非任意网页身份的绝对保证。

### 已知 AEB 开发集

全部 181 篇复测，原评分器、原 Markdown 规范化方法和固定 Firecrawl 2.11.0 全服务历史输出均保留：

| 输出 | F1 |
|---|---:|
| 上轮 Pagesift | 0.94816177 |
| 本轮 Pagesift | 0.95214520 |
| Firecrawl 固定参考 | 0.80475268 |

相对上轮 12 篇改善、5 篇下降、164 篇不变。**没有将平均提升写成逐页无退化。** Digg 从 0.64621 升至 0.98159，参考为 0.68922；推荐标题污染的因果反例已加入回归。正文范围与标题保留会影响体文金标评分，未采用“全部删标题”来追分。

| 原先落后站点 | 上轮 | 本轮 | 固定 Firecrawl |
|---|---:|---:|---:|
| nj.com | 0.95230 | 0.95230 | 0.95966 |
| digg.com | 0.64621 | 0.98159 | 0.68922 |
| bgr.com | 0.84501 | 0.84071 | 0.92120 |
| jaraguadosul.com.br | 0.16298 | 0.16298 | 0.20506 |
| nbcdfw.com | 0.75204 | 0.75204 | 0.79330 |
| politifact.com | 0.98923 | 0.98923 | 0.97789 |
| foxnews.com | 0.89767 | 0.89767 | 0.92537 |
| techcrunch.com | 0.98354 | 0.98354 | 1.00000 |
| panarmenian.net | 0.92727 | 0.92727 | 0.72771 |
| nytimes.com | 0.95829 | 0.95829 | 0.96079 |
| oregonlive.com | 0.98726 | 0.98726 | 0.99598 |
| nytimes.com | 0.98848 | 0.98848 | 0.98971 |
| wired.com | 0.93571 | 0.93571 | 0.94057 |
| beachbodyondemand.com | 0.99170 | 0.99170 | 0.99635 |

原始 14 篇落后列表中，已超过参考的从 2 篇变为 3 篇，仍有 11 篇。AEB 是已看过的开发集，本轮在它上面的多次调试不叫独立验收。完整逐篇变化 (local `results/core-next/summary.json`)。

### 新增上游金标与一次保留验证

Mozilla Readability 官方 `test/test-pages` 固定于 [`ab4027a8b37669745016869a37a504727992b2ba`](https://github.com/mozilla/readability/tree/ab4027a8b37669745016869a37a504727992b2ba/test/test-pages)，共 **130 对 source.html / expected.html**。没有缺失配对，没有与 AEB 的 HTML 字节哈希重复。先冻结来源分组：开发 78 篇/61 组，保留 52 篇/47 组，来源组不跨两侧。预先看过的 001 明确放开发集。

开发期间只评分 78 篇；冻结源码 `546b9e242d0b8f062845f5ad7ba7b02e4f3c12de` 后首次评分另外 52 篇，之后未修改产品源码。expected HTML 转可见文字，候选 Markdown 使用固定规范化，再按上游 AEB 四词 shingle 计算宏 P/R 的调和 F1。它是新增上游回归金标，包含不同网站与合成反例，不能当成真实网络总体准确率或全由真人盲标的新闻样本。

| 分组 | 上轮 Pagesift | 本轮 Pagesift | 固定 Essence 组件 |
|---|---:|---:|---:|
| 开发 78 | 0.91578978 | 0.91543312 | 0.87021339 |
| 保留 52 | 0.92884745 | 0.93310468 | 0.88142265 |

保留集本轮精度 0.907492、召回 0.960205；上轮为 0.899276 / 0.960430。开发集 F1 小幅下降约 0.00036，同样保留。

探索性来源组配对 bootstrap 2000 次（seed 20261022）：本轮减上轮的 95% 百分位区间为 [0.000312, 0.009690]；本轮减 Essence 组件为 [0.005336, 0.103898]。这是探索性不确定性描述，不冒称事先注册的显著性检验。

Essence 使用此前锁定的 v0.3.0 源码模块、原生 Rust 实现及薄适配器；130/130 输出完整，未挑选成功子集。这里比较正文组件，不比较服务吞吐或完整产品能力。其运行与编译存在资源共享，未据耗时做速度排名。

[拆分协议](../../bench/data/six/core-next/gold/protocol.json)、基线 (local `results/core-next/gold-baseline/summary.json`)、首次保留结果 (local `results/core-next/sealed-validation/summary.json`)、Essence 参考 (local `results/core-next/essence-validation/summary.json`)。本次保留集现已使用，后续调优不能继续称它为未见数据。

## 重定位：业务 ID 不变时允许标签变化

原有 Anchor 只在原标签内查身份，会漏掉 section→article，也会漏查跨标签重复 ID。本轮按整个文档的业务 ID 先保证唯一，标签只用于区分 Exact / Relocated。旧 CSS 命中其他条目仍拒绝，跨标签重复身份返回 Ambiguous。

同一组 18 个合成能力案例：上轮 **14/18**，本轮 **18/18**。新增标签变化、标签加选择器变化、跨标签重复 ID、旧选择器复用四种情况。仍要求稳定业务属性，不把缺少身份时的外观相似猜测当成可靠业务重定位。原始输入/输出 (local `results/core-next/relocation/summary.json`)。

## 增量核心：正文从检查点中移出

状态格式 version 1 只保存 source/item 身份及规范化 JSON 的 BLAKE3 摘要；对象键显式排序，避免下游启用 serde_json preserve_order 后因传输键顺序变化重发。完整条目仍先写入交付目标，确认后再 flush 页断点。旧版未版本化、包含完整 payload 的检查点在读取时迁移，保留游标、已见 ID、删除策略和元数据；未知未来版本拒绝打开。

同一真实 CLI / 本地 HTTP 协议：1000 条、每条 70000 字节正文，10 页，每页 100 条，单页低于抓取上限。相同配置和 32 MiB 状态上限，原版交付 **400 条后报上限错误**；本轮完成 **1000 条**，再扫描一轮不重发未变化条目。首轮状态日志实测 28011895 对 553000 字节；这是不同完成进度下含历史记录的日志体积，不冒充最终检查点大小、通用压缩比或公平吞吐排名。生产构建验证 (local `results/core-next/checkpoint-volume-release/summary.json`)。

另复制上轮真实 WordPress.org 未完成状态（原证据未修改），新程序成功从原下一页续取：事件 ID 从 1、2 变为 1、2、4，没有重发前两条。首次 ID 3 在该公开列表中不存在，所以没有将业务 ID 当作页码。请求未开启删除。迁移前记录 (local `results/core-next/live-migration/before.json`)、迁移后原始事件 (local `results/core-next/live-migration/events.jsonl`)。新版生产 CLI 的 503 恢复、更新、末页删除、无变化跳过与日志修复也重跑通过。

仍有边界：单来源单写者；至少一次交付而非跨目标事务；业务 ID 与摘要仍占内存，32 MiB 上限与来源分区要求保留；不包含 gallery-dl 的完整站点下载库。

## 交付与验证

- 完整 scripts/ci.sh 通过：fmt、Clippy、**557 项 Rust/文档测试**、Rust 1.88、cargo deny。
- 正文门槛 100KB 博客 1.88ms / 250KB wiki 6.29ms；本地爬取门槛 1200/1200 页、零错误、RSS 40MB。仅为本地门槛。
- 全部任务共享 8 个逻辑 CPU / 8GiB RAM 的 slice，任务 swap 0；最终 CI 峰值约 4.1GiB。没有干预其他应用或全局 swap。
- 生产二进制 SHA256 `98c1704c6350c00415044291e13c9ee133fedc12b4287d7f7609e23b8a1ffca9`。冻结源码与当前产品文件逐字节一致。源码包 (local `results/core-next/candidate-source.tar.gz`)、逐文件哈希 (local `results/core-next/source.json`)、机器摘要 (local `results/core-next/summary.json`)、CI 记录 (local `results/20261022T011305Z-core-next-full-ci-77f453/run.json`)。
- 本轮遇到并修复“标题绑定后仍被全页比例兜底覆盖”的合成回归及“完整 article 被 85% 下钻丢尾段”的 Medium 开发回归；失败日志全部保留，没有替换为成功记录。
- 仅在隔离副本创建源码快照，原仓库没有提交、推送或发布。

下一步差距仍明确：无文章声明/无语义标记的短正文定位、商品调用块与相关视频说明的保留边界，以及仍低于参考的 11 篇文章。不得把本次三个局部核心收益写成六个项目全功能领先。

已清理本轮闲置构建缓存，回收约 3.77GiB，当前磁盘可用约 28.62GiB；命名二进制和所有评测证据保留。清理记录 (local `results/core-next/cleanup.json`)。
