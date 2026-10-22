# 显式 HTTP Cookie 会话

新增能力由 Rust 实现：`pagesift-net::CookieSession`、`FetchRequest::with_session`，以及文件持有者 `pagesift::session::SessionFile`。默认请求仍无状态，没有自动共享的全局 Cookie jar。

CLI 会话请求使用独立的 HTTP 路径，不接入匿名抓取缓存，也不会自动升级浏览器或切换身份。

```bash
# 将 URL 替换为自己获授权的、返回 Set-Cookie 的 HTTP 会话入口。
pagesift session-fetch 'https://accounts.example.com/session' \
  --state ./private-session.json --format json

# 新进程使用同一来源、同一个状态文件继续请求。
pagesift session-fetch 'https://accounts.example.com/account' \
  --state ./private-session.json --format md
```

`--format json` 输出 Markdown 和不含 Cookie 值的会话统计，包括来源、Cookie 数量、请求次数、连续错误数、退役状态和到期时间。默认输出 Markdown；`--full` 保留整页。

## 来源与身份

会话绑定到确切的 `(scheme, host, port)`，不同身份使用不同的 `Arc<CookieSession>` 或状态文件。

- 同源路径间自动接收及发送符合条件的 Cookie，重定向中收到的 Cookie 可用于随后同源跳转。
- 请求初始 URL 与会话来源不一致时拒绝使用；重定向到其他主机、协议或端口时不发送会话 Cookie、请求身份头或配置身份头，也不接收其 Cookie。
- 会话内只接受 host-only 或 Domain 恰好等于原始主机的 Cookie，不支持父域 Cookie 跨子域共享。
- 会话拥有自己的 Cookie 字段：不会混合主机静态 Cookie；同时显式提供请求 Cookie 头会报错。同源 Authorization 等非 Cookie 凭证仍按调用方请求/配置发送，因此调用方应明确选择对应身份的凭证。

这比浏览器 Domain / 跨端口 Cookie 行为更严格，**不是完整浏览器 Cookie 兼容实现**。跨子域 SSO、第三方/分区 Cookie、浏览器 SameSite 上下文和 storage state 仍未实现。Cookie 的基本路径、过期和头部语义参考 [RFC 6265](https://www.rfc-editor.org/rfc/rfc6265.html)，本实现仅覆盖文中明确的子集。

## Cookie 与生命周期

支持路径边界匹配、默认 Path、同名不同路径排序、原位替换、Max-Age 优先于 Expires、到期删除、Secure、HttpOnly 的保存及恢复，以及 __Secure-/__Host- 的额外约束。__Host- 要求 HTTPS、Secure、显式 Path=/ 且无 Domain。

默认限制：会话存活 24 小时、最多 10000 次请求、连续错误上限 3；最多 64 个 Cookie，单 Cookie 输入 4096 字节、总量约 64KiB、发出的 Cookie 头最多 8192 字节、快照最多 256KiB。Rust `SessionConfig` 可在实现的硬上限内配置。

一次逻辑请求仅占一次次数预算，重定向跳数不重复计数；并发预占采用互斥状态保护。401/403/429/5xx、网络或正文读取失败、未完成就丢弃的流记为失败，正常完成重置连续错误数。到期、退役或预算耗尽后拒绝新请求，不会自动换一个账号继续请求；已在途请求不承诺被立即取消。

`session-fetch` 还使用既有正文拦截页检测器：识别到 HTTP 200 挑战页等访问屏障时返回错误并持久退役。低层 Rust 网络 API 不做正文含义识别，应用可自行调用 `retire()`。

只有显式 `--reset-session` 才新建会话并清空逻辑状态；该标志不等于自动重新登录。CLI 目前只执行 GET，不提交账号密码表单或执行 OAuth 流程。获授权的初始 Cookie 也可通过 Rust `set_cookie()` 明确种入。

## Rust 使用

```rust,no_run
# async fn demo(fetcher: &pagesift_net::Fetcher) -> Result<(), Box<dyn std::error::Error>> {
use std::sync::Arc;
use pagesift_net::{CookieSession, FetchRequest, SessionConfig, Url};
let origin = Url::parse("https://accounts.example.com/")?;
let session = Arc::new(CookieSession::new(&origin, SessionConfig::default())?);
let response = fetcher.fetch(
    FetchRequest::new(origin.as_str()).with_session(session.clone())
).await?;
println!("status={} cookies={}", response.status, session.stats().cookies);
# Ok(()) }
```

低层 Fetcher 执行 SSRF/DNS/凭证和重定向检查；需要 robots、限速和冷却时通过 `Policy::fetch` 或 `Policy::stream_held` 传递同一请求。会话不会绕过这些策略。robots 请求不绑定这个身份的会话 Cookie。

## 显式持久化

```rust,no_run
# fn demo() -> anyhow::Result<()> {
use pagesift::session::SessionFile;
let state = SessionFile::open(
    "./private-session.json", &"https://accounts.example.com/".parse()?,
    Default::default(), false,
)?;
let session = state.session();
// 将 session.clone() 绑定到请求；等待需要持久保存的请求结束。
state.save()?;
# Ok(()) }
```

快照包含真实 Cookie，存储为明文 JSON；Unix 新文件以 0600 创建，并拒绝已有的组/其他用户可读写状态文件。文件和临时路径拒绝符号链接，独占文件锁限制一个写者。保存采用同步临时文件、原子替换，Unix 再同步目录；Debug 输出脱敏，不应将 `SessionSnapshot::as_bytes()` 输出到日志。

CLI 在一次请求结束后显式保存，失败请求已经收到的同源响应 Cookie 和健康状态也会保存。进程在保存前被终止时，只恢复到上一次保存：没有承诺每条 Set-Cookie 立刻落盘、在途请求次数跨崩溃精确计账或断电保证。Rust 调用方应在所有需要纳入快照的并发请求结束后调用 save。

来源错误、格式损坏、过大快照、未知版本或权限过宽时默认拒绝，不悄悄覆盖旧身份。退役状态跨进程保留。没有加密、防本机所有者篡改或浏览器 Cookie 导入；Windows ACL 和非 Linux 平台尚未实机验收。

当前尚未自动接入 HTTP API、crawl、collect、download 或浏览器池。后续接线需同时处理身份缓存键、并发会话租借和持久状态，不能只将 Cookie 挂到匿名缓存请求上。
