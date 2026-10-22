# 第二轮：页面发现差距改进 — 2026-10-22

本轮把第一轮已复现的发现缺口转成自研 Rust 修改。它延续[总方案](../../docs/comparison-plan-2026-10-22.md)的基线、定位、修复和三方复测闭环；完整独立发布验收尚未完成。

## 修改与作用范围

- 保留原有 `<a href>`、`<area href>`，新增 iframe/frame 文档地址、`hx-get`/`data-hx-get` 和 HTML `link rel=next/prev`。
- HTTP 提取新增 `Link` 响应头的 next/prev 导航提示；解析多个字段、URI 中逗号和引号内分隔符，跳过改变上下文的 anchor 参数。
- 去重、fragment 清除和 HTTP(S) 过滤沿用原逻辑；爬虫继续使用原有作用域、robots、深度及数量限制。
- 不执行 HTMX 写方法，不把 preload、canonical 或脚本里的任意字符串作为此次新增导航来源。

这会扩充公开 `links` 输出，map、普通 crawl 和聚焦 crawl 复用该输出。响应头新增行为适用于 HTTP 提取路径；直接浏览器模式及 HTTP 升级到浏览器后的响应头继承并未在本轮补齐。frame 内嵌内容提取与发现 frame URL 是两个不同功能。

## 改前、改后与对手

同一受控源站、相同 7 个内容入口、串行执行，访问日志评分：

| 工具/配置 | 找到内容入口 | robots 禁止路径请求 |
|---|---:|---:|
| Pagesift 第一轮候选（二进制冻结） | 3/7 | 0 |
| Pagesift 本轮候选 | 6/7 | 0 |
| Crawlee Cheerio 常规 enqueueLinks handler | 2/7 | 0 |
| Katana 原生配置 | 6/7 | 1 |
| Katana 显式排除本构造禁用路径 | 6/7 | 0 |

新增找回 `/frame`、`/htmx`、`/link-header`。剩余 `/script-route` 只是脚本字符串，双方此配置均未找回。显式禁用路径排除不能称为 Katana 通用 robots 支持；Crawlee 的 handler 范围不能称为库的上限。此图只有 7 个入口，不代表真实互联网发现率。

完整命令、访问日志与分数 (local `results/round2/discovery/summary.json`)。原基线与候选二进制 SHA-256 见 回放摘要 (local `results/round2/replay/summary.json`)，本轮累计补丁 SHA-256：`9b85efda19fab94b90262b0d363d821aff57e1c34c503b0cacdd20a922319c9e`。

## 回归与兼容

- 同一回放源站，181 篇原 AEB HTML + 12 个图片构造样例，原输入逐份核对 SHA-256。双方各 193 次有效 HTTP API 输出，正文 Markdown **193/193 字节相同**、图片 **193/193 相同**；42 个页面 links 有变化，符合本轮扩充发现范围的目标。没有重新把旧 Firecrawl 输出当作本轮实时测速。
- 真实 HTTP 集成测试检查 frame、HTMX、响应头分页被发现，同时确保 robots 禁止路径、路径作用域外、POST、preload 和脚本字符串均未被请求。
- 原有深度、数量限制及聚焦爬取测试仍通过；新增 Link 语法测试覆盖逗号、引号、anchor、非 HTTP 协议和无效字段。
- 全量本地 CI：**539 项 Rust 测试及文档测试通过**，fmt、clippy、Rust 1.88、cargo-deny、正文与爬取性能门槛均通过。完整日志 (local `results/20261021T232645Z-round2-full-ci-ef23b5/stdout.log`)。
- 官方 Python/JS SDK 共 **17 项通过**：Python (local `results/round2/sdk/python.json`)、JS (local `results/round2/sdk/js.json`)。

首个新增测试把旧式 frame 放在普通 body 内，HTML5 解析器正确忽略了无效结构；已修正测试期望，保留失败日志。开发过程中改动了文件的初次构建/测试记录被 runner 标为 source_changed_during_run，不作为最终冻结验证；后续固定源码的构建和回归均已重新执行。

## 来源与后续工作单

参考来源是锁定 Katana v1.8.0 (`35267ac5c8ff1db9694a319d0eb466ed97b0969f`) 的 `pkg/engine/parser/parser.go`：headerLinkParser、bodyFrameTagParser、bodyIframeTagParser、bodyHtmxAttrParser。用于确认缺失输入类型；本轮没有复制 Go 实现，Rust 逻辑和测试由本项目自研。Katana 的更多方法/资源发现不是本轮目标。

本轮实施优先收敛到有明确边界的发现缺口。其余第二轮候选保留为明确工作单，未冒充已经修复：

1. **正文 14 篇落后案例**：nj.com 的嵌入推文保留、digg.com 的推荐噪声等见逐页清单 (local `results/firecrawl-full-scores/diff.json`)。应先建立跨站引用块与推荐块金标，再修改通用清理规则；源码与冻结 HTML 的候选原因记录在正文诊断 (local `results/round2/content-triage.json`)，尚未用消融确认；不能以输出更长或同一验证集调分代替质量改进。
2. **改版重定位**：Scrapling 默认阈值在上一轮相似干扰负例中误选。增加 Rust 模糊匹配前，需要扩大删除/近似商品/同价异名等拒绝样本，固定字段正确率及误匹配门槛；本轮未新增模糊匹配接口。
3. **发现后续**：独立站点/路径族、直接浏览器响应头、GET 返回非正文的成本与候选来源追踪。脚本字符串提取必须另设误入率与请求预算门槛。

## 资源与复现

继续使用共享 systemd slice：8 个逻辑 CPU、8 GiB RAM、0 swap，构建任务最多 8。未重新拉取 Firecrawl 镜像，未停止用户应用。调试编译关闭 debug 符号和增量缓存，结束后只清理确认不再使用的本轮 debug 产物。

```bash
python3 bench/six/run.py --id round2-discovery-repeat --timeout 240 -- \
  python3 bench/six/discovery.py bench/six/results/my-round2-discovery \
  bench/data/six/baseline-bin/pagesift-round1 bench/data/six/rust-target/release/pagesift
python3 bench/six/run.py --id round2-replay-repeat --timeout 240 -- \
  python3 bench/six/replay_regression.py bench/six/results/my-round2-replay \
  bench/data/six/baseline-bin/pagesift-round1 bench/data/six/rust-target/release/pagesift
```

使用新的输出目录，输入、对手版本和依赖沿用 [README](README.md) 及锁文件。第一轮报告保留其当时结论，本轮结果单独存放。无提交、推送或发布操作。
