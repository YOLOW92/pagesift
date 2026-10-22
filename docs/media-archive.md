# 可恢复的文件归档

`pagesift download` 将一个直接文件 URL 写入本地归档。它是自研 Rust 的文件传输和提交核心，尚不包含 gallery-dl 的站点 extractor 库、HLS/DASH、OAuth、浏览器 Cookie 导入或后处理插件。

```bash
pagesift download 'https://files.example.org/media.zip' \
  --source example --id item-42 --revision 1 \
  --out ./media-archive --max-bytes 268435456
```

下载失败后，用相同命令重新运行。JSON 输出包含 `path`、`bytes`、`blake3`、`resumed_bytes`、`cached`。已知正确文件哈希时可传 `--expected-blake3 <64位十六进制>`，只有校验通过才提交。`[fetch].timeout_secs` 控制网络总时限；CLI 不自动循环重试。

文件身份为 `(source, id, revision)`，用结构化元组的 BLAKE3 摘要生成文件名，避免用户输入成为路径。完成文件以 `.bin` 保存，不猜扩展名。URL 改变不会改变已完成条目身份；内容版本改变时必须提供新的 `revision`。归档命中前会重新核对实际文件长度和哈希，不能只凭文件名跳过。

## 续传和提交

- 请求使用 `Accept-Encoding: identity`，原始流直接写磁盘，不受 HTML 抓取的内存正文上限影响。代理模式也关闭响应自动解压，避免编码字节和 Range 偏移不一致。
- 每累计约 1MiB 成功写入，先同步文件，再原子替换带前缀长度和哈希的检查点。发生正常网络错误也保存已确认写入的前缀。
- 重启验证已同步前缀的哈希。文件中超出检查点的尾部被丢弃；无强 ETag、URL 改变或显式 `--restart` 时从头开始。`--restart` 只丢弃未完成传输，不覆盖已完成条目。
- 续传同时发送 `Range` 和 `If-Range`。206 必须有与偏移、总长和原 ETag 一致的范围；200 被视为完整替换，先清空旧前缀；416 不被当成成功。
- 长度越界、HTTP 错误、HTML 登录页、非 identity 编码、校验失败均不提交最终文件。没有预期哈希或可信总长时，只能验证传输协议与本地一致性，不能证明服务器给的是语义上正确的媒体。
- 完成校验并同步后先保存 ready 收据，再用不覆盖既有文件的硬链接提交最终文件名；可恢复“ready 收据已同步但最终文件尚未出现”的中断。硬链接保证同一文件系统内的原子可见性；不支持硬链接的目标文件系统会报错，不能静默降级为非原子复制。
- 同一归档目录使用操作系统 advisory lock，只允许一个写者，进程退出后锁自动释放。目录需由调用方信任和管理。

范围处理依据 [RFC 9110 If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-range) 与 [Content-Range](https://www.rfc-editor.org/rfc/rfc9110.html#name-content-range)。实现使用自己的状态机，不复制竞品下载代码。

## Rust API

```rust,no_run
# async fn demo(policy: &pagesift_policy::Policy) -> anyhow::Result<()> {
use pagesift::download::{Archive, DownloadRequest};
let mut archive = Archive::open("./media-archive")?;
let result = archive.download(policy, &DownloadRequest {
    source: "example".into(), id: "item-42".into(), revision: "1".into(),
    url: "https://files.example.org/media.zip".parse()?,
    max_bytes: 256 * 1024 * 1024,
    expected_blake3: None, restart: false,
}).await?;
println!("{} {}", result.path.display(), result.blake3);
# Ok(()) }
```

`Policy::stream_held` 提供底层原始字节流和并发 permit，必须持有 permit 到读取和写入结束。`Archive` 默认遵守 robots、限速、冷却、SSRF、DNS 和每跳重定向检查；主机凭证沿用配置。低层 `Fetcher::fetch_stream` 本身不执行 robots，和原来的 `Fetcher::fetch` 一样由 Policy 负责这一层。

Linux 已执行真实 SIGKILL 恢复和并发写者拒绝测试；macOS、Windows 和其他文件系统还未实机验收。Unix 会同步目录；非 Unix 未承诺同样的断电持久性。进程终止恢复不等同于突然掉电或所有异步取消边界保证。当前 `collect` 的分页交付还没有自动与文件下载提交组成一个事务。

当前公开对照与边界见 [评测报告](../bench/six/REPORT.md)；早期逐命令记录保留在本地归档。
