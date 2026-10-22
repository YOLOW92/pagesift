# 自研 Rust 文件归档与脚本导航交付（2026-10-22）

本轮完成两项产品核心增量：可恢复的直接文件归档，以及保守的内联脚本导航/GET 发现。它们分别在固定 gallery-dl 和 Katana 的受控任务上出现可复查优势。六项目全功能领先尚未达成：能力清单中的 40 类能力仍有 11 类明确未实现，另有部分实现和未完成对比的项目。不会把这次局部优势改写成全功能领先。

## 已落地实现

1. `crates/pagesift-net/src/fetch.rs` 提供 raw `FetchStream`：共享原抓取的 DNS/SSRF、重定向、凭证和超时检查，流式返回字节；代理另用禁用自动解压的客户端。原 HTML 抓取仍走原解码语义。
2. `pagesift-policy::Policy::stream_held` 结合 robots、冷却、限速及并发 permit，供文件传输在整个读写阶段持有名额。
3. `crates/pagesift/src/download.rs` 自研 Archive，新增 `pagesift download` CLI：稳定来源/条目/版本身份、BLAKE3 前缀校验、强 ETag/If-Range 续传、显式字节上限、ready 收据和不覆盖既有文件的原子提交、跨进程归档锁。
4. `crates/pagesift-content/src/script_links.rs` 自研限额词法扫描：只发现明确字面量导航和默认 GET；接入正式 links/crawl 输出，拒绝动态拼接、未知/写选项、注释及伪代码字符串。每脚本限制 1MiB、65536 tokens、256 个候选，文档总内联预算 2MiB。

本次没有把 GPL 的 gallery-dl 下载器复制到双许可产品中。实现按 HTTP 语义自行编写，参考竞品源码用于能力梳理和运行原生对照；新增依赖 fs2 用于操作系统文件锁，现有依赖版本未整体升级。

## 文件传输对照

使用 gallery-dl 1.32.17 原生 Directlink/HTTP 下载路径及 Pagesift 原生 CLI。同一确定性 ZIP 载荷、同一 HTTP 服务端，每工具从独立目录开始；首请求传送 786432 字节后断线，自动重试设为 0，然后再次启动命令。文件正确性按完整字节比较，不只看退出码、长度或文件存在。

| 场景 | Pagesift | gallery-dl 默认下载配置 |
|---|---|---|
| 断线后资源不变 | 续传，完整字节正确 | 续传，完整字节正确 |
| 断线后同一 URL 内容与 ETag 改变 | If-Range 失配后完整替换，字节正确 | 再次命令退出 0，但文件含旧前缀与新后缀，完整字节不匹配 |

服务端按照请求的 Range/If-Range 返回当前表示。逐请求记录表明 Pagesift 发送了旧强 ETag 的 If-Range；该固定 gallery-dl 默认路径只发送 Range。修改 gallery-dl 配置或下载策略可能改变结果，本测试不覆盖其所有选项。它证明一个资源变化时的续传一致性差异，不证明站点覆盖或总体下载性能领先。

范围与验证器的依据：[RFC 9110 If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-range)、[Content-Range](https://www.rfc-editor.org/rfc/rfc9110.html#name-content-range)。Pagesift 对未知验证器选择重下；416、错误范围、校验不符或 HTML 登录响应不当作完成。

真实进程实验另在 1048576 字节已同步断点处发出 SIGKILL，下一独立进程从同一偏移继续并得到正确文件；活跃写者存在时，第二 CLI 因归档锁拒绝。进程中断不等于断电、跨文件系统或所有异步取消边界保证。

原始协议、请求头、命令、退出码与日志：final-cli (local `results/media-core/final-cli/summary.json`)。单元/集成测试还覆盖弱/缺少 ETag、200 替换、错误 Content-Range、416、预期校验失败、部分尾部修复、成品篡改、ready 收据恢复、代理非 identity 编码和 robots 重定向拒绝。

## 脚本入口发现对照

八个目标包含入口本身、普通链接、location 赋值、assign、replace、window.open、默认 fetch GET、Unicode 转义路径。另设 POST、未知选项、注释、字符串伪代码、动态拼接、未使用常量、数据脚本与正则伪代码等拒绝候选。所有工具看同一份 HTML，候选走原爬虫正常边界。

| 实现 | 覆盖目标 | 访问的拒绝候选 | 非 GET 请求 |
|---|---:|---:|---:|
| 上轮完整 CLI 546b9e2（发现基线） | 2/8 | 0 | 0 |
| 最终 Pagesift | 8/8 | 0 | 0 |
| 固定 Katana 1.8.0 -jc | 7/8 | 7 | 0 |

Katana 用途包括广义端点发现，额外 GET 候选不能写成它执行了 POST；三者在本图均没有写请求。随机 URL 前缀用于隔离本次实例，语法模板仍是受控合成数据，不冒充独立真实站点。初次对照后最终二进制重跑得到相同结论。

当前没有外部 JS bundle 遍历、完整 AST、跨脚本常量求值或浏览器网络端点回灌。第一轮只有 `const article="/script-route"` 的旧入口仍不在已知 GET/导航范围内，不能把本图 8/8 写成旧缺口已全部清零。

最终逐请求结果 (local `results/media-core/final-navigation/summary.json`)、[实现边界](../../docs/script-discovery.md)。

## 回归、资源与审计

- 最终完整 CI：577 项 Rust/文档测试、fmt、Clippy、MSRV 1.88、cargo-deny 通过。
- 正文性能门槛：100KB 博客 1.89ms、250KB wiki 6.22ms；爬取门槛 1200/1200、0 错误、3686 页/秒、RSS 41MB。只作本机门槛，不作跨工具速度排名。
- 官方 Python 4.30.1 与 @mendable/firecrawl-js@4.28.1 共 17 项冒烟检查通过：scrape/map/crawl/batch、轮询、取消和部分错误。不是完整 SDK 全参数兼容证明。
- AEB 181 篇 Markdown 与上轮已验证结果逐字节相同；正文 F1 沿用 0.955629，剩 5 篇落后参考未被本轮清零。没有重新运行 PDF/RAG 的全部历史语料或启动六家完整服务。
- 共享 slice 总计限制 8 个逻辑 CPU、8GiB RAM，task swap 为 0，磁盘最低预留 10GiB。最终 CI 单服务采样峰值约 2047MiB；检查结束磁盘约剩 23GiB。没有停止用户应用或清理全局 swap。
- 原仓库 HEAD 未变，未提交、推送或发布；保留前序工作区所有改动。新旧源码、二进制身份、原始失败结果、对照与最终结果分别保存。

一次原生对照误用了 gallery-dl 不支持的 --timeout，根本没有发送 HTTP 请求，不计为竞品失败；修正为 downloader.http.timeout 后重跑并要求工具确实发起至少两次媒体请求。最初 CI 有一次 Clippy late-initialization 失败，修正后完整复跑。日志均保留，排除说明在机器摘要。

cargo-deny 通过，但仍显示原锁文件 Tokio 1.54.0 的 yanked 警告；该版本非本轮引入，也没有通过升级全部依赖掩盖这一记录。

最终 CI (local `results/20261022T022717Z-media-discovery-final-ci-100afd/run.json`)、源码归档与哈希 (local `results/media-core/source.json`)、机器摘要 (local `results/media-core/summary.json`)。

## 全功能领先的剩余工作

[40 类能力清单](FULL-CAPABILITIES.md) 与 [机器清单](full-capabilities.json) 固定六家源码版本和文件哈希。未实现项不从目标分母删掉；已有实现不自动填成领先。

后续优先：会话与身份隔离 → 明确可拒绝的模糊重定位接口 → 外部 JS / 浏览器网络前沿 → 站点适配与分页交付/文件提交事务 → HLS/DASH 和后处理 → 持久浏览器交互/Agent 生命周期，再做独立质量与完整服务成本验收。原有反自动化绕过相关产品边界仍需明确取舍，不能据此声称覆盖竞品全部行为。

当前下载支持直接文件 URL、同目录硬链接提交和单写者归档；不等同于 gallery-dl 的媒体站点库。只有 Linux 完成实机测试；Windows、macOS、其他文件系统及长期运行尚未验收。
