//! 不建 DOM 的字节扫描：判断"值不值得为短页面再开一次浏览器"够用了。
//!
//! 两个扫描都只在需要时做（调用方在短页面上才调），大页面不白扫。

/// 页面里有没有 `<script`（不区分大小写）。
pub fn has_script_tag(html: &str) -> bool {
    html.as_bytes()
        .windows(b"<script".len())
        .any(|w| w.eq_ignore_ascii_case(b"<script"))
}

/// 页面本身几乎没有正文，只是用脚本跳到另一个地址。这样的页面可以给导航
/// 设一个按本页标签页获取耗时缩放的上限；真正的 JS 应用不适用。
pub fn has_client_redirect(html: &str) -> bool {
    const PATTERNS: &[&[u8]] = &[
        b"location.href",
        b"location.replace",
        b"location.assign",
        b"window.location",
    ];
    let bytes = html.as_bytes();
    PATTERNS
        .iter()
        .any(|p| bytes.windows(p.len()).any(|w| w.eq_ignore_ascii_case(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_are_case_insensitive() {
        assert!(has_script_tag("<p>x</p><SCRIPT src=a.js>"));
        assert!(!has_script_tag("<p>scripted</p>"));
        assert!(has_client_redirect("<script>Window.Location='/x'</script>"));
        assert!(!has_client_redirect("<a href=/x>location</a>"));
    }
}
