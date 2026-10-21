//! 爬取前沿的质量规则：哪些 URL 是同一页的不同写法，哪些不值得抓，哪些是陷阱。
//!
//! - URL 清洗：去掉跟踪参数（utm_*、fbclid、gclid…）和会话 ID（`;jsessionid=`、PHPSESSID…）。
//!   参数名单取自 courlan 的跟踪参数规则和 Heritrix 的会话 ID 规则，只收确定是跟踪或会话的名字，
//!   `ref`、`source`、`id` 这类也可能决定页面内容的参数不动。
//! - 抓取前过滤：图片、音视频、压缩包、字体、脚本样式这类转不成文本的文件；
//!   同一段路径连续重复三次以上（Heritrix 的 PathologicalPath 规则，典型是坏掉的相对链接
//!   `more/` 越叠越长）、路径层级过深、URL 过长。
//! - 按 URL 模板学习陷阱（自研）：把 URL 归成模板（路径里的数字和长 ID 换成占位符，
//!   查询只留参数名），记下每个模板抓到的页面有多少是重复内容（全文相同，或者只差数字，
//!   比如日历的"下一月"、翻到头的空列表页、排序筛选的组合）。一个模板抓了几页、大多是重复，
//!   就把它降级：它的 URL 排到所有正常 URL 之后。只降级不丢弃，正常页面抓完、上限还没到时
//!   仍然会抓它们；模板后来抓到新内容，比例下降，又会恢复正常。
//! - URL 改写规则学习（DUST，Bar-Yossef 等 "Do not crawl in the DUST"，以及 Dasgupta 等
//!   KDD 2008 的 URL 改写规则去重）：两个 URL 的正文完全相同时，比较它们的差别——多了一个查询参数
//!   （`print=1`），或者路径末尾多了一段（`index.html`、`more/`）——记成候选规则；同一条规则
//!   被两对不同的页面证实后生效，之后发现的 URL 先按规则改写再去重，同一页的其他写法不再抓。
//!   参数规则默认只去掉特定的值（`page=1` 和不带参数相同，不代表 `page=2` 也相同）；
//!   同一个参数名有两种以上取值都被证实，才去掉整个参数。

use std::collections::{HashMap, HashSet, VecDeque};

use pagesift_net::Url;

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
const TRACKER_PREFIXES: &[&str] = &["utm_", "mtm_", "itm_"];
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
/// 路径最多多少段（Heritrix TooManyPathSegments 的默认值）。
const MAX_SEGMENTS: usize = 20;
const MAX_URL_LEN: usize = 2048;
/// 同一段（或连续几段）路径最多连续出现几次。
const MAX_REPEATS: usize = 2;

/// 一条改写规则要被几对不同的页面证实才生效。
const RULE_SUPPORT: usize = 2;

/// 模板至少抓了几页才判断，重复页占多少比例时降级。
const DEMOTE_MIN_FETCHED: u32 = 4;
const DEMOTE_SHARE: f32 = 0.75;

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
pub(crate) fn clean(u: &mut Url) {
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
pub(crate) fn skip(u: &Url) -> bool {
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
pub(crate) fn shape(u: &Url) -> String {
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
pub(crate) fn shape_key(u: &Url) -> u64 {
    fnv(shape(u).bytes())
}

/// 页面内容和已抓页面的关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seen {
    New,
    /// 和之前某页全文相同：那一页的 URL。
    Duplicate(String),
    /// 和同一模板下之前的某页只差数字（日历、页码、计数器）。
    Templated,
}

#[derive(Default)]
struct ShapeStats {
    fetched: u32,
    repeated: u32,
    /// 这个模板下各页去掉数字后的正文哈希
    skeletons: HashSet<u64>,
}

/// 一次爬取里学到的内容重复情况。内存随抓过的页面数线性增长（每页一个哈希和一个 URL）。
/// 从重复页面对里学到的 URL 改写规则，都限定在一个主机上。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Rule {
    /// 去掉这个查询参数（只限这个值）
    DropPair {
        host: String,
        name: String,
        value: String,
    },
    /// 去掉这个查询参数（任何值）
    DropName { host: String, name: String },
    /// 路径末尾去掉这一段（如 `index.html`、`more/`），只要前面还剩下 `/`
    StripSuffix { host: String, suffix: String },
}

#[derive(Default)]
struct Rules {
    /// 每条候选规则被哪些页面（规范写法的哈希）证实过
    evidence: HashMap<Rule, HashSet<u64>>,
    active: HashSet<Rule>,
    /// 同一参数名被证实可以去掉的取值（取值够多时整个参数名生效）
    values: HashMap<(String, String), HashSet<String>>,
    /// 生效的后缀规则，按主机（改写时按顺序反复尝试）
    suffixes: HashMap<String, Vec<String>>,
}

impl Rules {
    fn support(&mut self, rule: Rule, witness: u64) {
        if self.active.contains(&rule) {
            return;
        }
        let ev = self.evidence.entry(rule.clone()).or_default();
        ev.insert(witness);
        if ev.len() >= RULE_SUPPORT {
            self.evidence.remove(&rule);
            if let Rule::StripSuffix { host, suffix } = &rule {
                self.suffixes
                    .entry(host.clone())
                    .or_default()
                    .push(suffix.clone());
            }
            if let Rule::DropPair { host, name, value } = &rule {
                let vals = self.values.entry((host.clone(), name.clone())).or_default();
                vals.insert(value.clone());
                if vals.len() >= 2 {
                    self.active.insert(Rule::DropName {
                        host: host.clone(),
                        name: name.clone(),
                    });
                }
            }
            self.active.insert(rule);
        }
    }

    /// `long` 和 `short` 正文完全相同，`short` 是更简短的写法：记下把 `long` 变成 `short` 的规则。
    fn learn(&mut self, long: &Url, short: &Url) {
        let (Some(host), Some(h2)) = (long.host_str(), short.host_str()) else {
            return;
        };
        if host != h2 || long.scheme() != short.scheme() {
            return;
        }
        let witness = fnv(short.as_str().bytes());
        let lq: Vec<(String, String)> = long
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let sq: Vec<(String, String)> = short
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if long.path() == short.path() {
            // 查询参数多了几个（其余完全相同）
            if lq.len() > sq.len() && sq.iter().all(|p| lq.contains(p)) {
                for (name, value) in lq.iter().filter(|p| !sq.contains(p)) {
                    self.support(
                        Rule::DropPair {
                            host: host.to_string(),
                            name: name.clone(),
                            value: value.clone(),
                        },
                        witness,
                    );
                }
            }
        } else if lq == sq
            && let Some(suffix) = long.path().strip_prefix(short.path())
            && short.path().ends_with('/')
            && !suffix.is_empty()
        {
            self.support(
                Rule::StripSuffix {
                    host: host.to_string(),
                    suffix: suffix.to_string(),
                },
                witness,
            );
        }
    }

    /// 按生效的规则改写 URL；改了返回 true。
    fn apply(&self, u: &mut Url) -> bool {
        if self.active.is_empty() {
            return false;
        }
        let Some(host) = u.host_str().map(str::to_string) else {
            return false;
        };
        let mut changed = false;
        if let Some(suffixes) = self.suffixes.get(&host) {
            let mut path = u.path().to_string();
            loop {
                let before = path.len();
                for sfx in suffixes {
                    if path.len() > sfx.len()
                        && path.ends_with(sfx.as_str())
                        && path[..path.len() - sfx.len()].ends_with('/')
                    {
                        path.truncate(path.len() - sfx.len());
                    }
                }
                if path.len() == before {
                    break;
                }
            }
            if path != u.path() {
                u.set_path(&path);
                changed = true;
            }
        }
        if u.query().is_some() {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let kept: Vec<&(String, String)> = pairs
                .iter()
                .filter(|(name, value)| {
                    !self.active.contains(&Rule::DropName {
                        host: host.clone(),
                        name: name.clone(),
                    }) && !self.active.contains(&Rule::DropPair {
                        host: host.clone(),
                        name: name.clone(),
                        value: value.clone(),
                    })
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
                changed = true;
            }
        }
        changed
    }
}

#[derive(Default)]
pub(crate) struct Learner {
    exact: HashMap<u64, String>,
    rules: Rules,
    shapes: HashMap<u64, ShapeStats>,
    /// 每有一个模板从正常变成降级就加一，队列据此判断要不要重新分层
    generation: u64,
}

impl ShapeStats {
    fn demoted(&self) -> bool {
        self.fetched >= DEMOTE_MIN_FETCHED
            && self.repeated as f32 >= DEMOTE_SHARE * self.fetched as f32
    }
}

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// 正文的比较用形式：只看字母数字，空白和标点的差异不算。
fn text_bytes(markdown: &str) -> impl Iterator<Item = u8> + '_ {
    markdown
        .bytes()
        .filter(|b| !b.is_ascii() || b.is_ascii_alphanumeric())
}

impl Learner {
    /// 记下抓到的一页，返回它是新内容还是重复。
    pub(crate) fn observe(&mut self, url: &Url, markdown: &str) -> Seen {
        let exact = fnv(text_bytes(markdown));
        let skeleton = fnv(text_bytes(markdown).map(|b| if b.is_ascii_digit() { b'0' } else { b }));
        let stats = self.shapes.entry(shape_key(url)).or_default();
        let was = stats.demoted();
        stats.fetched += 1;
        let seen = if let Some(first) = self.exact.get(&exact) {
            if let Ok(f) = Url::parse(first) {
                // 短的那个算规范写法
                if f.as_str().len() <= url.as_str().len() {
                    self.rules.learn(url, &f);
                } else {
                    self.rules.learn(&f, url);
                }
            }
            Seen::Duplicate(first.clone())
        } else if !stats.skeletons.insert(skeleton) {
            Seen::Templated
        } else {
            Seen::New
        };
        if seen == Seen::New || matches!(seen, Seen::Templated) {
            self.exact.entry(exact).or_insert_with(|| url.to_string());
        }
        if seen != Seen::New {
            stats.repeated += 1;
        }
        if !was && stats.demoted() {
            self.generation += 1;
        }
        seen
    }

    /// 按学到的改写规则把 URL 改成规范写法；改了返回 true。
    pub(crate) fn rewrite(&self, u: &mut Url) -> bool {
        self.rules.apply(u)
    }

    /// 这个 URL 的模板是否已经降级（大多抓到重复内容）。
    pub(crate) fn demoted(&self, url: &Url) -> bool {
        self.demoted_key(shape_key(url))
    }

    fn demoted_key(&self, key: u64) -> bool {
        self.shapes.get(&key).is_some_and(ShapeStats::demoted)
    }
}

/// 广度优先的两层队列：模板降级的 URL 排在降级层，正常层空了才出队。
/// 正常层按主机分队列（同 Heritrix 的每主机 work queue），主机之间轮流出队，跳过在途请求已满的主机
/// （同 Scrapy 的 DownloaderAwarePriorityQueue），一个主机排得再长也不会挡住其他主机。
/// 总长度由调用方按爬取上限控制；满了以后新来的正常 URL 可以挤掉降级层最后面的一个。
#[derive(Default)]
pub(crate) struct Tiered {
    normal: HashMap<String, VecDeque<(Url, usize, u64)>>,
    /// 正常层里还有 URL 的主机，轮流出队的顺序
    ring: VecDeque<String>,
    normal_len: usize,
    low: VecDeque<(Url, usize, u64)>,
    /// 上次把正常层里已降级的 URL 挪走时，学习器的 generation
    sorted_at: u64,
}

fn host_of(u: &Url) -> String {
    u.host_str().unwrap_or_default().to_string()
}

impl Tiered {
    pub(crate) fn len(&self) -> usize {
        self.normal_len + self.low.len()
    }

    pub(crate) fn push(&mut self, learner: &Learner, url: Url, depth: usize) {
        let key = shape_key(&url);
        if learner.demoted_key(key) {
            self.low.push_back((url, depth, key));
            return;
        }
        let host = host_of(&url);
        let q = self.normal.entry(host.clone()).or_default();
        if q.is_empty() {
            self.ring.push_back(host);
        }
        q.push_back((url, depth, key));
        self.normal_len += 1;
    }

    /// 下一个要抓的 URL，第三项表示是否来自降级层。正常层里模板已降级的顺手挪到降级层。
    /// `busy(host)` 为真的主机这次跳过（在途请求已到上限）。
    /// `allow_low` 为 false 时只出正常层：还有正常页面在抓或等重试，它们可能发现新的正常链接，
    /// 这时候抓降级的 URL 会白白占掉上限。
    pub(crate) fn pop(
        &mut self,
        learner: &Learner,
        allow_low: bool,
        busy: impl Fn(&str) -> bool,
    ) -> Option<(Url, usize, bool)> {
        for _ in 0..self.ring.len() {
            let host = self.ring.pop_front()?;
            if busy(&host) {
                self.ring.push_back(host);
                continue;
            }
            let q = self.normal.get_mut(&host).expect("ring hosts have queues");
            let mut found = None;
            while let Some(item) = q.pop_front() {
                self.normal_len -= 1;
                if learner.demoted_key(item.2) {
                    self.low.push_back(item);
                } else {
                    found = Some((item.0, item.1, false));
                    break;
                }
            }
            if q.is_empty() {
                self.normal.remove(&host);
            } else {
                self.ring.push_back(host);
            }
            if found.is_some() {
                return found;
            }
        }
        if !allow_low || self.low.front().is_some_and(|i| busy(&host_of(&i.0))) {
            return None;
        }
        self.low.pop_front().map(|(u, d, _)| (u, d, true))
    }

    /// 队列满了、又来了一个正常 URL：从降级层末尾挤掉一个，返回被挤掉的 URL（调用方从去重表里删掉它，
    /// 以后还能再进来）。降级层是空的就返回 `None`，新 URL 不入队。
    pub(crate) fn evict_low(&mut self, learner: &Learner) -> Option<Url> {
        if self.sorted_at != learner.generation {
            self.sorted_at = learner.generation;
            for q in self.normal.values_mut() {
                let before = q.len();
                let mut keep = VecDeque::with_capacity(before);
                for item in q.drain(..) {
                    if learner.demoted_key(item.2) {
                        self.low.push_back(item);
                    } else {
                        keep.push_back(item);
                    }
                }
                self.normal_len -= before - keep.len();
                *q = keep;
            }
            self.normal.retain(|_, q| !q.is_empty());
            let normal = &self.normal;
            self.ring.retain(|h| normal.contains_key(h));
        }
        self.low.pop_back().map(|(u, _, _)| u)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Url, usize)> {
        self.ring
            .iter()
            .flat_map(|h| self.normal[h].iter())
            .chain(&self.low)
            .map(|(u, d, _)| (u, *d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn cleaned(s: &str) -> String {
        let mut x = u(s);
        clean(&mut x);
        x.to_string()
    }

    #[test]
    fn strips_trackers_and_sessions() {
        assert_eq!(
            cleaned("https://a.com/p?utm_source=x&id=7&fbclid=IwAR1&utm_medium=y#top"),
            "https://a.com/p?id=7"
        );
        assert_eq!(
            cleaned("https://a.com/p/4/;jsessionid=ABC123?x=1"),
            "https://a.com/p/4/?x=1"
        );
        assert_eq!(cleaned("https://a.com/p;JSESSIONID=ABC"), "https://a.com/p");
        assert_eq!(
            cleaned("https://a.com/?PHPSESSID=0123456789abcdef0123456789"),
            "https://a.com/"
        );
        assert_eq!(
            cleaned("https://a.com/?sid=8dd3dede98a7626cbf4e1c23217573da"),
            "https://a.com/"
        );
        // 短的 sid、ref、source 可能决定内容，不动
        assert_eq!(
            cleaned("https://a.com/x?sid=3&ref=main&source=rss"),
            "https://a.com/x?sid=3&ref=main&source=rss"
        );
        assert_eq!(cleaned("https://a.com//docs///a"), "https://a.com/docs/a");
        assert_eq!(
            cleaned("https://a.com/a?b=1&a=2"),
            "https://a.com/a?b=1&a=2"
        );
    }

    #[test]
    fn skips_binaries_and_traps() {
        assert!(skip(&u("https://a.com/media/1.JPG")));
        assert!(skip(&u("https://a.com/files/x.tar.gz")));
        assert!(!skip(&u("https://a.com/doc.pdf")));
        assert!(!skip(&u("https://a.com/v1.2/guide")));
        assert!(!skip(&u("https://a.com/p/5/more/more/")));
        assert!(skip(&u("https://a.com/p/5/more/more/more/")));
        assert!(skip(&u("https://a.com/a/b/a/b/a/b/")));
        assert!(!skip(&u("https://a.com/a/b/a/b/c")));
        let deep = format!(
            "https://a.com/{}",
            (0..25)
                .map(|i| format!("s{i}"))
                .collect::<Vec<_>>()
                .join("/")
        );
        assert!(skip(&u(&deep)));
    }

    #[test]
    fn shapes_group_templated_urls() {
        assert_eq!(
            shape(&u("https://a.com/events?month=2026-11")),
            shape(&u("https://a.com/events?month=1999-01"))
        );
        assert_eq!(
            shape(&u("https://a.com/p/12/")),
            shape(&u("https://a.com/p/7/"))
        );
        assert_ne!(
            shape(&u("https://a.com/p/12/")),
            shape(&u("https://a.com/q/12/"))
        );
        assert_eq!(
            shape(&u("https://a.com/list?sort=new&view=grid")),
            shape(&u("https://a.com/list?view=list&sort=az"))
        );
    }

    #[test]
    fn learns_rewrite_rules_from_duplicate_pairs() {
        let mut l = Learner::default();
        let page = |n: usize| {
            format!(
                "Story {n} about {}",
                ["cats", "dogs", "owls", "bees", "rats"][n]
            )
        };
        for n in 0..5 {
            l.observe(&u(&format!("https://a.com/p/{n}/")), &page(n));
        }
        // 一对证据不够
        l.observe(&u("https://a.com/p/0/index.html"), &page(0));
        l.observe(&u("https://a.com/p/0/?print=1"), &page(0));
        l.observe(&u("https://a.com/p/0/?page=1"), &page(0));
        let mut x = u("https://a.com/p/3/index.html?print=1");
        assert!(!l.rewrite(&mut x));
        // 第二对证实后生效
        l.observe(&u("https://a.com/p/1/index.html"), &page(1));
        l.observe(&u("https://a.com/p/1/?print=1"), &page(1));
        l.observe(&u("https://a.com/p/1/?page=1"), &page(1));
        assert!(l.rewrite(&mut x));
        assert_eq!(x.as_str(), "https://a.com/p/3/");
        // 只去掉证实过的取值
        let mut y = u("https://a.com/p/3/?page=2");
        assert!(!l.rewrite(&mut y));
        let mut z = u("https://a.com/p/3/?page=1&q=x");
        assert!(l.rewrite(&mut z));
        assert_eq!(z.as_str(), "https://a.com/p/3/?q=x");
        // 其他主机不受影响
        let mut w = u("https://b.com/p/3/index.html");
        assert!(!l.rewrite(&mut w));
        // 越叠越长的相对链接：规则反复套用
        l.observe(&u("https://a.com/p/2/more/"), &page(2));
        l.observe(&u("https://a.com/p/4/more/"), &page(4));
        let mut m = u("https://a.com/p/9/more/more/index.html");
        assert!(l.rewrite(&mut m));
        assert_eq!(m.as_str(), "https://a.com/p/9/");
        // 反过来先抓到长写法也一样能学
        let mut r = Learner::default();
        r.observe(&u("https://c.com/d/1/index.php"), "one");
        r.observe(&u("https://c.com/d/1/"), "one");
        r.observe(&u("https://c.com/d/2/index.php"), "two");
        r.observe(&u("https://c.com/d/2/"), "two");
        let mut v = u("https://c.com/d/7/index.php");
        assert!(r.rewrite(&mut v));
        assert_eq!(v.as_str(), "https://c.com/d/7/");
    }

    #[test]
    fn hosts_take_turns_and_busy_hosts_are_skipped() {
        let l = Learner::default();
        let mut q = Tiered::default();
        for p in [
            "https://a.com/1",
            "https://a.com/2",
            "https://a.com/3",
            "https://b.com/1",
            "https://c.com/1",
        ] {
            q.push(&l, u(p), 0);
        }
        let order: Vec<String> =
            std::iter::from_fn(|| q.pop(&l, true, |_| false).map(|x| x.0.to_string())).collect();
        assert_eq!(
            order,
            [
                "https://a.com/1",
                "https://b.com/1",
                "https://c.com/1",
                "https://a.com/2",
                "https://a.com/3"
            ]
        );
        for p in ["https://a.com/4", "https://b.com/2"] {
            q.push(&l, u(p), 0);
        }
        assert_eq!(
            q.pop(&l, true, |h| h == "a.com").unwrap().0.as_str(),
            "https://b.com/2"
        );
        assert!(
            q.pop(&l, true, |h| h == "a.com").is_none(),
            "only a busy host is left"
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.pop(&l, true, |_| false).unwrap().0.as_str(),
            "https://a.com/4"
        );
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn learns_traps_from_repeated_content() {
        let mut l = Learner::default();
        let cal = |m: &str| {
            (
                u(&format!("https://a.com/events?month={m}")),
                format!("# Events in {m}\n\nNo events scheduled."),
            )
        };
        for (i, m) in ["2026-10", "2026-11", "2026-12", "2027-01"]
            .iter()
            .enumerate()
        {
            let (url, md) = cal(m);
            assert!(!l.demoted(&url));
            let seen = l.observe(&url, &md);
            assert_eq!(seen == Seen::New, i == 0, "{m}");
        }
        assert!(l.demoted(&cal("2027-02").0));
        // 内容各不相同的模板不降级
        for i in 0..10 {
            let url = u(&format!("https://a.com/p/{i}/"));
            assert_eq!(
                l.observe(
                    &url,
                    &format!(
                        "Story about topic {}",
                        [
                            "cats", "dogs", "owls", "bees", "rats", "cows", "pigs", "elks", "yaks",
                            "emus"
                        ][i]
                    )
                ),
                Seen::New
            );
        }
        assert!(!l.demoted(&u("https://a.com/p/99/")));
        // 全文相同：报告第一次出现的 URL
        assert_eq!(
            l.observe(&u("https://a.com/p/3/?print=1"), "Story about topic bees"),
            Seen::Duplicate("https://a.com/p/3/".into())
        );
    }
}
