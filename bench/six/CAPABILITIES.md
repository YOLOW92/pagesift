# 能力边界兑现：实现、验证与未完成项

本次把前三项边界继续推进到产品代码。**业务身份重定位与增量采集已落地限定范围内的可用接口；正文差距只部分补齐，仍不能把前三项全部标为完成。** 所有实现为本仓库的 Rust 代码，没有把原生工具基准适配脚本当成产品能力。

## 1. 正文：总体改善，但 14 篇尚未全部追平

在固定 AEB 181 篇、原始评分器、原始 Firecrawl 2.11.0 自托管输出上复测：

| 输出 | F1 |
|---|---:|
| 改动前 Pagesift | 0.93926185 |
| 本次 Pagesift | 0.94816177 |
| 固定 Firecrawl 参考 | 0.80475268 |

17 篇改善，其余不变，无单篇退化。修复了紧凑 social/embed 引用容器被整体清除的问题；未标发布时间但有单一充分正文区域的文章声明可被结构证据确认；支持 article_body 容器及 CMS 通用子标题，避免严格段落白名单丢失章节问题。

规则没有写入站点名、文章 ID 或预期评分。新增结构测试覆盖不同引用包装、分享控件/推荐块负例、未标日期的正文区域、多个文章卡片和过短卡片。已有“只有 article 声明不足以启用正文模式”的测试调整为区分无声明和具有充分结构佐证的声明，这是明确的产品行为变更。

这些是开发集回归数据：本轮已检查过这些文章，不将复测称作独立盲评。新增合成结构测试也不冒称真人标注的跨站金标。参考输出沿用第四轮固定数据，本轮未重新启动 Firecrawl 全栈；候选使用产品正文转换器，比较正文质量，不比较服务耗时。

| 原先落后站点 | 改前 F1 | 本次 F1 | Firecrawl F1 | 状态 |
|---|---:|---:|---:|---|
| nj.com | 0.7811 | 0.9523 | 0.9597 | 仍有差距 |
| digg.com | 0.5485 | 0.6462 | 0.6892 | 仍有差距 |
| bgr.com | 0.8450 | 0.8450 | 0.9212 | 仍有差距 |
| jaraguadosul.com.br | 0.1630 | 0.1630 | 0.2051 | 仍有差距 |
| nbcdfw.com | 0.7520 | 0.7520 | 0.7933 | 仍有差距 |
| politifact.com | 0.9439 | 0.9892 | 0.9779 | 已超过 |
| foxnews.com | 0.8977 | 0.8977 | 0.9254 | 仍有差距 |
| techcrunch.com | 0.9835 | 0.9835 | 1.0000 | 仍有差距 |
| panarmenian.net | 0.7150 | 0.9273 | 0.7277 | 已超过 |
| nytimes.com | 0.9509 | 0.9583 | 0.9608 | 仍有差距 |
| oregonlive.com | 0.9873 | 0.9873 | 0.9960 | 仍有差距 |
| nytimes.com | 0.9836 | 0.9885 | 0.9897 | 仍有差距 |
| wired.com | 0.9357 | 0.9357 | 0.9406 | 仍有差距 |
| beachbodyondemand.com | 0.9917 | 0.9917 | 0.9964 | 仍有差距 |

**14 篇中 2 篇超过参考，12 篇仍落后。** 其中既有仍需研究的提取问题，也有输出范围差异：例如有些金标不计文章标题，有些同时计入商品推荐或关联视频文字。没有用删掉所有标题、保留所有推荐来强行消掉分差。后续仍需独立的跨站人工引用/推荐块金标，才能把这一项标为完全兑现。

全部原始输出与逐篇结果 (local `results/capabilities/verified/summary.json`)。

## 2. Rust 业务身份重定位

新增 pagesift-extract 的 `Anchor` / `Relocation`：锚点可序列化；定位返回 Exact、Relocated、Missing 或 Ambiguous，并提供原生 NodeRef 继续抽取。即便旧 CSS 仍匹配，也会重新检查身份；重复业务 ID 拒绝选取，不以“第一个候选”消歧。

第四轮 12 个固定校准案例从精确 CSS 的 9/12，变为身份锚点的 12/12；加入重复业务 ID、价格更新两个案例后为 **14/14**。这是明确业务身份条件下的合成能力验证，不是对任意网页自适应准确率的估计。Scrapling 默认模糊匹配与此接口要求的输入不同，不据此宣称通用优越。

业务属性与标签必须稳定。没有业务 ID、ID 改变或元素标签改变时返回缺失；不具备 Scrapling 那种在身份消失后继续按外观相似度猜测的能力。该拒绝边界有意保留。

[复现适配器](../../crates/pagesift-extract/examples/relocate_jsonl.rs)、原始结果 (local `results/capabilities/verified/relocated.jsonl`)。

## 3. 自动身份、API 断点、更新/删除和适配

- `discover_items` 自动读取 data-item-id、data-product-id、itemid；重复或冲突身份返回错误。
- `Collection` 将已见条目与下一页 URL 保存在一条 CRC 保护的状态记录内；先确认交付，再 flush 断点。
- CLI `collect` 支持真实 JSON API、可配置 JSON 指针，以及 WordPress REST 原生 id / 总数 / 总页数协议；HTML 身份发现也可接入同一增量状态机。
- 完整末页且明确 authoritative 才推断删除；默认只新增/更新。错误、截断、字段缺失、分页循环、跨源分页、重复 ID、总数改变、不完整末页均不作为删除证据。
- 输出采用持久 JSONL 事件日志，恢复时清理不完整尾行、拒绝完整损坏行；`--restart` 可以放弃失效扫描，保留上次已提交清单。

真实 HTTP 集成测试通过；另以实际 CLI 启动五个独立进程，确认页数预算断点、503 不前进、恢复页 2 不重抓页 1、URL 改变仍更新同一 ID、末页删除、无变化不交付、日志半行修复。CLI 实跑 (local `results/capabilities/cli-release/summary.json`)。另对 WordPress.org 官方新闻 API 进行了两次独立 CLI 调用，每次预算一页，实际得到两个不同业务 ID，第二次从持久断点续页；未开启删除，也未把仅两页的验证称为全站同步。真实 API 结果 (local `results/capabilities/live-wordpress/summary.json`)。

局限：每来源单写者；跨目标交付至少一次，需要幂等；单检查点上限 32 MiB，大集合需分区；WordPress 总数检查不能替代服务端一致性快照。没有扩展为 gallery-dl 的完整站点库、媒体下载器或登录系统。站点适配目前覆盖 WordPress 协议及显式通用映射，没有宣称覆盖任意网站。

[完整使用方法与故障语义](../../docs/incremental-collection.md)。WordPress 协议依据 [官方分页说明](https://developer.wordpress.org/rest-api/using-the-rest-api/pagination/)。

## 验证、资源与可审查交付

- 最终 `scripts/ci.sh`：fmt、Clippy、**550 项 Rust/文档测试**、Rust 1.88、cargo deny 全部通过。
- 性能门槛：100KB 博客 1.83ms（上限 8ms）；250KB wiki 6.14ms（上限 30ms）；本地爬取门槛 1200/1200 页、零错误、RSS 39MB。仅为本地门槛，不作跨工具速度排名。
- 最终检查日志：run.json (local `results/20261022T004858Z-boundary-reviewed-ci-0956ad/run.json`)。前一轮全测试失败于旧 article 模式预期，已解释并更新测试范围；早期 HTTP 测试误用了不存在的限速配置字段，修正后完整重跑。中途修改源码的那次运行保留为 source_changed_during_run，不计最终通过。
- 全部重负载共享 8 个逻辑 CPU / 8GiB RAM、0 task swap 的 slice；最终 CI 峰值约 1.9GiB，更早全测试约 3.2GiB。未清理用户其他进程或原有缓存目录。
- 冻结快照 `a1d834ef02142adb8480ebaf60794b1e5713912c` 仅提交在隔离副本；原仓库没有提交、推送或发布。源码包 (local `results/capabilities/candidate-source.tar.gz`)、逐文件源码哈希 (local `results/capabilities/source.json`)。

当前交付状态：第二、三项在上文界定范围内实现并验证；第一项有可复现改善，**仍未完成全部差距消除和独立跨站金标验收**。

本轮闲置构建缓存已清理，回收约 5.30 GiB，磁盘当前可用约 29.38 GiB。命名二进制、源码快照和全部测试证据保留，未动原有 target。清理记录 (local `results/capabilities/cleanup.json`)。
