#!/usr/bin/env python3
"""Render the bounded Rust editorial-core delivery report from retained evidence."""
import json
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];OUT=ROOT/'bench/six/results/editorial-core'
a=json.loads((OUT/'sealed/audit.json').read_text());v=json.loads((OUT/'validation/summary.json').read_text());u=json.loads((OUT/'validation/uncertainty.json').read_text());source=json.loads((OUT/'source.json').read_text())
closed=[r for r in a['prior_loss_cases'] if r['sealed-aeb']<r['firecrawl'] and r['final']>=r['firecrawl']];remaining=[r for r in a['prior_loss_cases'] if r['final']<r['firecrawl']]
lines=['# Rust 正文边界核心交付与竞争差距（2026-10-22）','',
'本次已修改自研 Rust 正文核心，并完成全量检查。原来仍低于固定 Firecrawl 参考的 11 篇文章，有 6 篇现在超过该参考，剩 5 篇。AEB 181 篇 F1 从 0.952145 升到 0.955629；这属于已使用数据上的开发回归收益。额外 1355 页的首次运行没有证明相对旧版的整体改善。不能将这些结果写成六个项目全功能领先。','',
'## 交付代码','',
'- `crates/pagesift-content/src/editorial.rs`：无声明短文定位；紧凑商品调用块、操作步骤中的购买链接、视频说明与嵌入引用的局部保护；显式归因推荐列表识别。','- `extract.rs`：严格文章过滤接入局部保护并按节点缓存；隐藏和硬过滤保持生效。','- `markdown.rs`：允许符合条件的正文附件输出 div 中的文字，避免价格、商品说明和图注因段落白名单丢失。','- `lib.rs`：无声明短文在默认只取正文模式下可进入独立区域；本次推断区域同时约束 Markdown、plain text、clean HTML。','- `tests/editorial_regions.rs`：12 个测试覆盖标题归属和新增边界，包含多个候选放弃收窄、全页模式、推荐商品卡片、隐藏视频、播放列表、社交加载占位符和链接目录等反例。','',
'所有规则使用结构、文字、链接和上下文证据，不含站点名或评测 ID 分支；本次新增逻辑为自研 Rust，没有把竞品核心代码直接复制到项目中。具体阈值与覆盖限制见 `docs/editorial-core.md`。','',
'## 原来 11 篇落后文章中的进展','',
'下面按单页 AEB F1 记录；“超过”仅指这些固定输入和固定评分，不代表该站点全站或统计稳定的全面领先。','',
'| 页面 | 修改前 | 修改后 | Firecrawl 固定参考 |','|---|---:|---:|---:|']
for r in closed:lines.append(f"| {r['site']} | {r['sealed-aeb']:.6f} | {r['final']:.6f} | {r['firecrawl']:.6f} |")
lines += ['', '市政短文由整页约 6991 字符缩到 1051 字符，主要正文完整保留。BGR 恢复商品标题、价格和购买说明；NBC/Fox 恢复视频附属说明；Wired 恢复来源联系引用并剔除显式推荐列表；健身文章恢复步骤内商品购买链接。','',
'剩余 5 篇仍低于参考，不能标记为清零：','', '| 页面 | 当前 F1 | 参考 F1 | 已核实差异 |','|---|---:|---:|---|']
notes={'nj.com':'保留标题、联系作者/订阅段；另有嵌入文字间距和标记差异','techcrunch.com':'当前输出保留文章标题，正文标注不含标题','nytimes.com':'标题/导语、署名、编辑来信/社交段或广告文字的保留边界','oregonlive.com':'当前输出保留文章标题，正文标注不含标题'}
for r in remaining:lines.append(f"| {r['site']} ({r['id'][:8]}) | {r['final']:.6f} | {r['firecrawl']:.6f} | {notes[r['site']]} |")
lines += ['', '这些差异没有被当成已解决。特别是标题、合法署名与正文附件的处理需要明确产品口径；本次没有通过删除全部标题/署名、保留全部推荐或修改评分器来追求逐页胜出。后续仍需处理订阅/编辑来信等页尾边界，并用新数据验证，不能把“没有明显整段遗漏”解释为质量等同。','',
'## 完整回归与首次验证','',
'| 数据 | 修改前 | 修改后 | 对照 |','|---|---:|---:|---:|',
f"| AEB 181 篇（开发回归，官方四词 shingle 指标） | {a['aeb_f1']['sealed-aeb']:.6f} | {a['aeb_f1']['final']:.6f} | Firecrawl {a['aeb_f1']['firecrawl']:.6f} |",
f"| WCXB 已使用的 511 页（回归，官方逐页 word F1 均值） | {a['wcxb']['before']:.6f} | {a['wcxb']['after']:.6f} | 本轮未重跑其他工具 |",
f"| WCXB 剩余 1355 页首次运行（同一数据集来源） | {v['before']['f1']:.6f} | {v['after']['f1']:.6f} | Essence 组件 {v['essence']['f1']:.6f} |",'',
'不同数据集的指标不可直接横比。修改前为上轮 546b9e2 源码快照对应适配器，修改后为当前工作区冻结的适配器；六个项目版本锁仍见 `tools.lock.json`。Firecrawl 使用此前保留的完整服务同输入结果，本轮没有重新启动服务。Essence 只比较固定稳定版的离线提取组件；Crawlee、Scrapling、Katana 和 gallery-dl 未在本轮重新跑完整功能，因此本轮不能对它们追加全面领先结论。','',
'首次运行前固定了 WCXB dev 全部候选，排除与旧 AEB/Readability/WCXB 测试相同 URL 或 HTML 的 142 页，保留全部 1355 页，不按输出补样。它仍与旧数据共享作者和部分域名，不能叫作新的官方独立测试集。协议、源码与二进制哈希在 `results/editorial-core/validation/protocol.json`。','',
'每个工具 2 个 worker、每页 10 秒，使用相同 HTML 和相同 Markdown 转文本规则。失败按空输出保留在分母。新旧 Pagesift 均 0 错误；Essence 的 0587 页超时，未替换页面。去掉该超时页的敏感性检查仍为 Pagesift 0.842262、Essence 0.772697。','',
f"1355 页按 {u['hostname_groups']} 个 hostname 分组，2000 次配对 bootstrap：相对 Essence 的 F1 差为 {u['comparisons']['essence']['delta']:.6f}，描述性 95% 区间为 [{u['comparisons']['essence']['paired_hostname_bootstrap_95_percentile'][0]:.6f}, {u['comparisons']['essence']['paired_hostname_bootstrap_95_percentile'][1]:.6f}]。这支持固定版本提取任务上的平均优势，不是全功能结论。",'',
'相对修改前，1355 页有 1353 页评分不变、2 页下降，无新增得分页；均值降低约 0.000036。511 页也有 1 页下降，原因是新增保留的正文配图/视频说明不在标注范围内。这些下降已计入主结果，不据此宣称泛化改善。其他附件边界也可能仍误判。','',
'## 验证与资源','',
'- 最终 `scripts/ci.sh` 全部通过：564 项 Rust/文档测试、fmt、Clippy、MSRV 1.88、cargo-deny。','- 最终性能门槛：100KB 博客 2.00ms（限 8ms）、250KB wiki 6.84ms（限 30ms）；抓取门槛 1200/1200、0 错误、2269 页/秒、RSS 42MB。并行验证时的门槛结果不用于跨版本速度比较。','- 构建和评测使用共享 systemd slice，合计限 8 个逻辑 CPU、8GiB RAM、任务不使用 swap；磁盘保留 10GiB。未清理用户应用或全局 swap，未向原仓库提交或推送。','- 最终 CI 证据：`results/20261022T015155Z-editorial-sealed-ci-3d750e/run.json`。','- 新旧整组评分：`results/editorial-core/sealed/audit.json`；独立页面评分与不确定性：`validation/summary.json`、`validation/uncertainty.json`。','- 当前代码归档：`results/editorial-core/candidate-source.tar.gz`；各文件及适配器哈希：`results/editorial-core/source.json`。','',
'## 仍未证明的能力','',
'无声明短文目前要求标题对应与直接叙述文字，深层行内容器或多段正文仍可能无法定位。附件需要紧凑结构和明确证据，不能保证覆盖任意播放器、商品组件或推荐变体。既有严格文章模式的 plain text/clean HTML 范围仍未完全与 Markdown 统一。六项目的完整浏览器、会话、调度、站点适配、媒体下载和服务能力没有因本次正文改动而自动补齐。','']
(ROOT/'bench/six/EDITORIAL-CORE.md').write_text('\n'.join(lines));print('report written')
