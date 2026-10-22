//! Type 1 嵌入字体程序的内建编码解析。

/// 查找字节切片中的子切片位置。
pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Type1 明文段的 /Encoding。
pub(crate) fn type1(clear: &[u8]) -> Option<Vec<Option<String>>> {
    let at = find(clear, b"/Encoding")?;
    let rest = &clear[at + 9..];
    let head = &rest[..rest.len().min(40)];
    if find(head, b"StandardEncoding").is_some() {
        return None;
    }
    let mut out = vec![None; 256];
    let mut found = false;
    let end = find(rest, b"readonly def")
        .or_else(|| find(rest, b"currentdict end"))
        .or_else(|| find(rest, b"eexec"))
        .unwrap_or(rest.len());
    let text = String::from_utf8_lossy(&rest[..end]);
    let toks: Vec<&str> = text.split_ascii_whitespace().collect();
    for w in toks.windows(4) {
        if w[0] == "dup"
            && w[3] == "put"
            && let Ok(code) = w[1].parse::<usize>()
            && let Some(name) = w[2].strip_prefix('/')
            && code < 256
        {
            out[code] = Some(name.to_string());
            found = true;
        }
    }
    found.then_some(out)
}
