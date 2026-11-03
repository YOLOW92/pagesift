//! URL 清洗、抓取前过滤和 URL 模板（固定规则）。

use super::*;

/// 确定是跟踪用途的查询参数（小写比较）。
const TRACKERS: &[&str] = &[
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "yclid",
    "ysclid",
    "twclid",
    "ttclid",
    "li_fat_id",
    "igshid",
    "igsh",
    "mc_cid",
    "mc_eid",
    "_ga",
    "_gl",
    "_hsenc",
    "_hsmi",
    "__hstc",
    "__hssc",
    "__hsfp",
    "hsctatracking",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "vero_conv",
    "rb_clickid",
    "s_cid",
    "_openstat",
    "srsltid",
    "wt_mc",
    "pk_campaign",
    "pk_kwd",
    "pk_source",
    "pk_medium",
    "pk_content",
    "spm",
    "scm",
];
/// 以这些前缀开头的查询参数都是跟踪参数。
/// 带前缀的跟踪参数：utm_*、Matomo 的 mtm_*、内部链接归因的 internal_*、Eloqua 的 elq*。
const TRACKER_PREFIXES: &[&str] = &["utm_", "mtm_", "itm_", "internal_", "elq"];
/// 会话 ID 参数。`sid` 太短太常见，只在值像随机 ID 时才算（见 [`is_session`]）。
const SESSIONS: &[&str] = &[
    "jsessionid",
    "phpsessid",
    "sessionid",
    "session_id",
    "cfid",
    "cftoken",
    "zenid",
    "oscsid",
];

/// 转不成文本的文件扩展名：抓了也是白抓。PDF、HTML、纯文本、XML 这类照常抓。
const BINARY_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "ico", "svg", "tif", "tiff", "heic", "psd",
    "mp3", "mp4", "m4a", "m4v", "mov", "avi", "wmv", "flv", "mkv", "webm", "wav", "ogg", "oga",
    "ogv", "flac", "aac", "mid", "midi", "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar",
    "zst", "iso", "dmg", "exe", "msi", "apk", "ipa", "deb", "rpm", "jar", "woff", "woff2", "ttf",
    "otf", "eot", "css", "js", "mjs", "map", "wasm", "swf",
];
/// 路径最多多少段。
/// [策略] 安全上限，取 Heritrix TooManyPathSegmentsDecideRule 的默认值 20（Heritrix 3 文档）。
/// 不从数据学：单页拒绝门槛取分布分位数会按
/// 比例丢掉正常页面；学到的陷阱由 [`super::Learner`] 的模板降级处理。
const MAX_SEGMENTS: usize = 20;
/// [协议] Sitemaps 协议 0.9（sitemaps.org）：URL 必须少于 2,048 个字符；更长的 URL 站点自己
/// 也不会在 sitemap 里列出，多半是越叠越长的相对链接或塞进查询串的状态。
const MAX_URL_LEN: usize = 2048;
/// 同一段（或连续几段）路径最多连续出现几次。
/// [策略] 安全上限，取 Heritrix PathologicalPathDecideRule 的默认值 2（Heritrix 3 文档）。
const MAX_REPEATS: usize = 2;

fn is_session(name: &str, value: &str) -> bool {
    if SESSIONS.contains(&name) || name.starts_with("aspsessionid") {
        return true;
    }
    name == "sid" && value.len() >= 16 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_tracker(name: &str) -> bool {
    TRACKERS.contains(&name) || TRACKER_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// 清洗一个发现的 URL：去 fragment、路径里的会话参数、跟踪和会话查询参数、路径里多余的 `/`。
/// 其余查询参数保持原有顺序（排序只用于去重键，见 `crawl::normalize`）。
pub fn clean(u: &mut Url) {
    u.set_fragment(None);
    let path = u.path();
    let lower = path.to_ascii_lowercase();
    if lower.contains(";jsessionid=") || lower.contains(";phpsessid=") || path.contains("//") {
        let raw: Vec<&str> = path.split('/').skip(1).collect();
        let mut trailing = path.ends_with('/');
        let mut parts: Vec<&str> = Vec::with_capacity(raw.len());
        for (i, seg) in raw.iter().enumerate() {
            let low = seg.to_ascii_lowercase();
            let seg = match low.find(";jsessionid=").or_else(|| low.find(";phpsessid=")) {
                Some(c) => &seg[..c],
                None => seg,
            };
            if seg.is_empty() {
                // `/p/4/;jsessionid=…` 去掉会话参数后是 `/p/4/`
                trailing |= i + 1 == raw.len();
                continue;
            }
            parts.push(seg);
        }
        let mut out = format!("/{}", parts.join("/"));
        if trailing && !parts.is_empty() {
            out.push('/');
        }
        u.set_path(&out);
    }
    if u.query().is_some() {
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let kept: Vec<&(String, String)> = pairs
            .iter()
            .filter(|(k, v)| {
                let k = k.to_ascii_lowercase();
                !is_tracker(&k) && !is_session(&k, v)
            })
            .collect();
        if kept.len() != pairs.len() {
            if kept.is_empty() {
                u.set_query(None);
            } else {
                u.query_pairs_mut()
                    .clear()
                    .extend_pairs(kept.iter().map(|(k, v)| (k, v)));
            }
        }
    }
}

/// 不值得抓的 URL：二进制文件、路径陷阱、过深过长。
pub fn skip(u: &Url) -> bool {
    if u.as_str().len() > MAX_URL_LEN {
        return true;
    }
    let path = u.path();
    let last = path.rsplit('/').next().unwrap_or_default();
    if let Some((_, ext)) = last.rsplit_once('.')
        && ext.len() <= 5
        && BINARY_EXT.contains(&ext.to_ascii_lowercase().as_str())
    {
        return true;
    }
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segs.len() > MAX_SEGMENTS || repeats(&segs)
}

/// 有没有一段（或连续 1～3 段）路径连续重复超过 [`MAX_REPEATS`] 次。
fn repeats(segs: &[&str]) -> bool {
    for len in 1..=3 {
        let need = len * (MAX_REPEATS + 1);
        if segs.len() < need {
            break;
        }
        for start in 0..=segs.len() - need {
            let unit = &segs[start..start + len];
            if (1..=MAX_REPEATS).all(|r| &segs[start + r * len..start + (r + 1) * len] == unit) {
                return true;
            }
        }
    }
    false
}

/// URL 模板：主机 + 路径（数字串换成 `#`，含数字的长段换成 `*`）+ 排好序的查询参数名。
pub fn shape(u: &Url) -> String {
    let mut s = String::with_capacity(u.as_str().len());
    s.push_str(u.host_str().unwrap_or_default());
    for seg in u.path().split('/') {
        s.push('/');
        if seg.len() >= 16 && seg.bytes().any(|b| b.is_ascii_digit()) {
            s.push('*');
            continue;
        }
        let mut in_digits = false;
        for c in seg.chars() {
            if c.is_ascii_digit() {
                if !in_digits {
                    s.push('#');
                }
                in_digits = true;
            } else {
                in_digits = false;
                s.push(c);
            }
        }
    }
    if u.query().is_some() {
        let mut keys: Vec<String> = u.query_pairs().map(|(k, _)| k.into_owned()).collect();
        keys.sort();
        keys.dedup();
        s.push('?');
        s.push_str(&keys.join("&"));
    }
    s
}

/// [`shape`] 的 64 位哈希，队列里每个 URL 带着它，判断降级不用重算模板。
pub fn shape_key(u: &Url) -> u64 {
    fnv(shape(u).bytes())
}
