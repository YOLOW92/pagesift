# 自研 Rust HTTP 会话交付（2026-10-22）

本轮落地显式、同源隔离的 HTTP Cookie 会话。新增核心与测试均为 Rust，没有引入新的语言或运行时；只将仓库原有 serde/serde_json 加为网络 crate 的直接依赖，没有增加第三方 crate 版本。

## 已实现

- `pagesift-net::CookieSession`、`FetchRequest::with_session`：默认无状态，调用方显式共享一个身份的 Arc；请求初始来源不符时拒绝，外源重定向不携带 Cookie 和身份头，也不收下其 Cookie。
- Rust Cookie 状态机：路径边界/默认路径、排序和替换、删除、Max-Age/Expires、HTTPS Secure、HttpOnly 元数据、__Secure-/__Host- 约束，以及 Cookie 数量、总量、请求 Cookie 头和快照上限。
- 生命周期：固定到期时间、请求次数预算、并发预占、连续 HTTP/网络/正文失败退役；普通响应和 raw stream 共用生命周期语义。
- `SessionFile`：显式快照、来源/类型/版本/大小检查、单写者锁、Unix 0600 权限、同步临时文件后原子替换。错误输出及 Debug 不含 Cookie 值；明文快照只在明确调用保存时产生。
- `pagesift session-fetch`：跨原生 CLI 进程复用同源状态，输出 Markdown 或安全统计 JSON；过期/退役/预算耗尽不悄悄换身份，只有显式 reset 才创建新会话。HTTP 200 访问屏障被检测时返回错误并保存退役状态。

使用方法及严格语义见 [HTTP 会话文档](../../docs/http-sessions.md)。基本 Cookie 路径与过期处理参照 [RFC 6265](https://www.rfc-editor.org/rfc/rfc6265.html)，实现为自行编写的 Rust 受限状态机，不是将竞品 Cookie 库直接搬进来。

## 实际验证

新增 14 个 Rust 测试覆盖多个场景：

| 位置 | 本轮测试数 | 主要检查 |
|---|---:|---|
| net session 单元测试 | 6 | Cookie 路径、排序、替换、删除、过期、前缀、来源、容量、快照、寿命和退役 |
| 真实 HTTP session 集成 | 5 | 重定向 Cookie、跨主机/端口、显式与静态凭证边界、两个身份、无状态请求、正文错误、raw stream 和并发预算 |
| SessionFile 集成 | 2 | 保存重开、独占锁、原始来源、未知/损坏/过大输入和 Unix 权限 |
| 原生 CLI 子进程集成 | 1 | 多个独立进程中的 Alice/Bob 状态复用、连续 401 退役、显式重置、过期、外源拒绝和 200 挑战页退役 |

测试账号和端点是本地受控 fixture，不是第三方真实账户、OAuth 或生产环境登录验收。CLI 子进程使用生产二进制和真实 HTTP 服务，没有用测试适配器补 Cookie 行为。它们证明这些明确的生命周期和隔离规则，不构成对 Crawlee/Scrapling 的全面胜出估计。

最终完整 `scripts/ci.sh` 通过 **591 项 Rust/文档测试**，fmt、Clippy、MSRV 1.88 和 cargo-deny 均通过。门槛：100KB 博客 1.89ms、250KB wiki 6.23ms；爬取 1200/1200、0 错误、3703 页/秒、RSS 41MB。这些是本机门槛，不作跨工具性能排名。

- 官方 Python/JS SDK：17 项全部通过。
- AEB：181 篇 Markdown 与此前确认的正文输出逐字节一致；本轮不将原来剩余 5 篇文章的差距记为解决。
- 上轮下载对照完整复跑：资源不变和变更时的 Pagesift 文件字节均正确，SIGKILL 续传与并发写者拒绝仍通过。
- 上轮导航图完整复跑：最终 Pagesift 8/8，拒绝候选访问与非 GET 请求均为 0；只是原协议回归，没有冒称新盲评。

最终 CI (local `results/20261022T030036Z-session-final-ci-635dcb/run.json`)、机器摘要 (local `results/session-core/summary.json`)、SDK (local `results/session-core/sdk/python.json`)、下载回归 (local `results/session-core/media/summary.json`)、导航回归 (local `results/session-core/navigation/summary.json`)。

加入挑战页测试时有一次失败：测试 fixture 漏写 /challenge 路由，实际返回普通页面；已修复该 fixture，并完整复跑 CI。失败日志保留于 `results/20261022T030007Z-session-reviewed-ci-bfaad6/`，不当作最终通过。

## 边界与剩余工作

HTTP 会话与完整的浏览器会话池是不同层次。本实现严格绑定协议/主机/端口，仅接受 host-only 或与原主机完全相等的 Domain，不支持父域跨子域 SSO，也没有浏览器 SameSite/分区上下文。这比标准浏览器 Cookie 范围更窄，不能写成完整 RFC 或浏览器 Cookie 等价。

CLI 目前只执行 GET，不处理账号密码表单、OAuth、浏览器 Cookie 导入或浏览器 storage state。配置的同源非 Cookie 凭证仍由调用方负责与对应身份匹配；这里只对显式 jar 及外源跳转边界作保证，不猜用户身份。

持久化为明文私有文件，只有显式 save 后的状态保证恢复；保存前的进程终止可能丢失本次 Cookie/次数更新。未承诺在途请求跨崩溃精确计账、突然断电、Windows ACL 或非 Linux 平台行为。Rust 调用方需同步自己的并发请求与保存时机。

当前尚未把会话自动接入 crawl/collect/download/HTTP API 或浏览器池。后续需先设计身份对应的缓存键、会话租借和文件/分页交付语义，避免只加 Cookie 却复用匿名结果。

[40 类能力清单](FULL-CAPABILITIES.md) 将 Crawlee/Scrapling 的两项从“完全缺失”改为“部分实现”，**没有改成全功能领先**。仍有 9 类完全缺失，另有部分实现和未验证项；本轮没有重跑六家完整服务或宣布完整竞品已被超越。

## 资源和可审查性

全部构建/评测使用原有共享 slice，总计限制 8 个逻辑 CPU、8GiB 内存、0 task swap，磁盘预留 10GiB。最终 CI 采样峰值约 1245MiB。未停止用户应用、清理全局 swap、提交或推送原仓库。

源码包 (local `results/session-core/candidate-source.tar.gz`)、文件与二进制哈希 (local `results/session-core/source.json`) 保留本次候选；所有前序改动保留。检查结束磁盘剩余约 21GiB。cargo-deny 通过时仍显示此前锁定 Tokio 版本的 yanked 警告，该版本非本轮引入。
