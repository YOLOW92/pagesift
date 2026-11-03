//! 爬取范围：种子决定的主机和路径前缀、include/exclude 规则，以及从叶子种子自动放宽范围。

use super::*;

/// 爬取范围的路径前缀：目录部分。
///
/// - `/` → 无前缀（整站）
/// - `/docs/` → `/docs`
/// - `/docs/index.html` → `/docs`
/// - `/docs/python` → `/docs/python`（不像文件名的路径按它自己的前缀，和 Firecrawl 一致；
///   0.71–0.72 曾把它放大到 `/docs`，连 `/docs/java` 也爬进来）
/// - `/docs` → `/docs`（单段路径保留，避免变成整站）
fn scope_path(start: &Url) -> Option<String> {
    let path = start.path();
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if path.ends_with('/') {
        return Some(trimmed.to_string());
    }
    match trimmed.rsplit_once('/') {
        Some(("", last)) => {
            // 根目录下的单段路径：`/index.html` 这种文件名按整站处理；
            // `/docs` 这种栏目根保留自身前缀，避免把范围意外放大到整站。
            (!last.contains('.')).then(|| trimmed.to_string())
        }
        None => Some(trimmed.to_string()),
        // 只有文件名（`index.html`、`page.php`）才换成所在目录
        Some((parent, last)) if last.contains('.') => Some(parent.to_string()),
        Some(_) => Some(trimmed.to_string()),
    }
}

/// 爬取范围：哪些发现的 URL 可以进入队列。
pub struct Scope {
    host: String,
    allow_subdomains: bool,
    allow_external: bool,
    path_prefix: Option<String>,
    /// `auto` 模式下可以放宽到的祖先目录，由近到远，不含整站根；空表示不放宽。
    ancestors: Vec<String>,
    /// 种子页的证据表明它是叶子页之后放宽到其中一个祖先目录，只设一次。
    widened: OnceLock<String>,
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Scope {
    pub(super) fn new(
        start: &Url,
        allow_subdomains: bool,
        allow_external: bool,
        mode: CrawlScope,
        include: &[String],
        exclude: &[String],
    ) -> Result<Self, ApiError> {
        let compile = |ps: &[String]| -> Result<Vec<Regex>, ApiError> {
            ps.iter()
                .map(|p| {
                    Regex::new(p)
                        .map_err(|e| ApiError::invalid(format!("invalid path pattern {p:?}: {e}")))
                })
                .collect()
        };
        // 先取最窄的合理范围：种子以文件名结尾时（.../index.html）取所在目录，否则把种子路径本身
        // 当目录。`auto` 再看种子页的链接决定要不要放宽到某个祖先目录（见 `widen_from_seed`），
        // 永远不会自动放宽到整站根。
        let narrow = (mode != CrawlScope::Domain)
            .then(|| scope_path(start))
            .flatten()
            .filter(|p| !p.is_empty());
        let mut ancestors = Vec::new();
        if let Some(mut dir) = narrow.as_deref().filter(|_| seed_names_page(start)) {
            while let Some((up, _)) = dir.rsplit_once('/').filter(|(up, _)| !up.is_empty()) {
                ancestors.push(up.to_string());
                dir = up;
            }
        }
        let (path_prefix, ancestors) = match mode {
            CrawlScope::Domain => (None, Vec::new()),
            CrawlScope::Exact => (narrow, Vec::new()),
            CrawlScope::Auto => (narrow, ancestors),
            // 按日期分目录的文章（`/blog/2026/07/21/post/`）的上一级是年月日目录，没有意义：
            // 跳过日期目录取所在栏目；栏目就是整站根时爬整个主机
            CrawlScope::Parent if !ancestors.is_empty() => (
                ancestors.into_iter().find(|dir| !is_date_dir(dir)),
                Vec::new(),
            ),
            CrawlScope::Parent => (narrow, Vec::new()),
        };
        Ok(Self {
            host: start
                .host_str()
                .unwrap_or_default()
                .trim_start_matches("www.")
                .to_ascii_lowercase(),
            allow_subdomains,
            allow_external,
            path_prefix,
            ancestors,
            widened: OnceLock::new(),
            include: compile(include)?,
            exclude: compile(exclude)?,
        })
    }

    pub fn allows(&self, u: &Url) -> bool {
        if !matches!(u.scheme(), "http" | "https") {
            return false;
        }
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        let bare = host.trim_start_matches("www.");
        let same = bare == self.host;
        // 同一站点的任何子域名（可注册域名相同）：`iss.example.edu` 和 `www.example.edu`，
        // 不只是起点主机的后代
        let sub = pagesift_content::site_key(bare) == pagesift_content::site_key(&self.host);
        if !(same || (self.allow_subdomains && sub) || self.allow_external) {
            return false;
        }
        let path = u.path();
        if same
            && let Some(prefix) = self.widened.get().or(self.path_prefix.as_ref())
            && !under(path, prefix)
        {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(|r| r.is_match(path)) {
            return false;
        }
        !self.exclude.iter().any(|r| r.is_match(path))
    }

    /// 还能不能按种子页的证据放宽（`auto` 且还没放宽过）。
    pub(super) fn can_widen(&self) -> bool {
        !self.ancestors.is_empty() && self.widened.get().is_none()
    }

    /// 在当前范围外、但放宽后可能进入范围的 URL（同主机、在最远的祖先目录下，且通过 include/exclude）。
    /// 已经放宽过或不能放宽时恒为 false。
    pub(super) fn may_widen_to(&self, u: &Url) -> bool {
        let (Some(narrow), Some(top)) = (&self.path_prefix, self.ancestors.last()) else {
            return false;
        };
        if self.widened.get().is_some() || self.allows(u) {
            return false;
        }
        let host = u.host_str().unwrap_or_default();
        let path = u.path();
        host.trim_start_matches("www.")
            .eq_ignore_ascii_case(&self.host)
            && under(path, top)
            && !under(path, narrow)
            && (self.include.is_empty() || self.include.iter().any(|r| r.is_match(path)))
            && !self.exclude.iter().any(|r| r.is_match(path))
    }

    /// `auto`：用种子页的链接判断种子是不是叶子页。子页链接很少（< [`WIDEN_MAX_CHILDREN`]）时，
    /// 由近到远找第一个有足够同级链接（≥ [`WIDEN_MIN_SIBLINGS`]）的祖先目录放宽过去，返回说明原因的警告。
    /// 博客文章、新闻报道、文档里的一页都是这种形状；目录页、分类页有自己的子页，不放宽。
    /// 按日期分目录的博客（`/blog/2026/07/21/post/`）同一天往往没有别的文章，
    /// 所以不只看上一级：同级文章分散在 `/blog/2026/…` 下时放宽到最近的那一层。
    /// 种子页自己要有正文（去掉链接后至少 [`WIDEN_MIN_PROSE`] 个字母/数字）：只列链接的空分类页、
    /// 导航页不算叶子内容页。
    pub(super) fn widen_from_seed<'a>(
        &self,
        seed: &Url,
        markdown: &str,
        links: impl IntoIterator<Item = &'a Url>,
    ) -> Option<String> {
        let Some(narrow) = &self.path_prefix else {
            return None;
        };
        if self.ancestors.is_empty() || prose_chars(markdown) < WIDEN_MIN_PROSE {
            return None;
        }
        let seed_path = seed.path().trim_end_matches('/');
        let mut children: HashSet<&str> = HashSet::new();
        let mut outside: HashSet<&str> = HashSet::new();
        for u in links {
            let host = u.host_str().unwrap_or_default();
            if !host
                .trim_start_matches("www.")
                .eq_ignore_ascii_case(&self.host)
            {
                continue;
            }
            let path = u.path().trim_end_matches('/');
            if path == seed_path {
                continue;
            }
            if under(path, narrow) {
                children.insert(path);
            } else {
                outside.insert(path);
            }
        }
        if children.len() >= WIDEN_MAX_CHILDREN {
            return None;
        }
        // 祖先目录本身和它的首页是“上一级”链接（面包屑），不算同级
        let (dir, siblings) = self.ancestors.iter().find_map(|dir| {
            let n = outside
                .iter()
                .filter(|p| under(p, dir) && **p != dir.as_str() && !is_index_of(p, dir))
                .count();
            (n >= WIDEN_MIN_SIBLINGS).then_some((dir, n))
        })?;
        self.widened.set(dir.clone()).ok()?;
        Some(format!(
            "crawl_scope: widened path scope from {narrow}/ to {dir}/ (start page links {} page(s) below it and {siblings} sibling page(s)); use --scope exact to keep it narrow",
            children.len(),
        ))
    }

    /// 从断点继续时放宽状态不落盘：入过队的 URL 里有窄范围外的，说明上次已经放宽过，
    /// 恢复到能装下它们的最近的祖先目录。
    pub(super) fn restore_widened<'a>(&self, enqueued: impl IntoIterator<Item = &'a Url>) {
        let outside: Vec<&Url> = enqueued
            .into_iter()
            .filter(|u| self.may_widen_to(u))
            .collect();
        if outside.is_empty() {
            return;
        }
        if let Some(dir) = self
            .ancestors
            .iter()
            .find(|dir| outside.iter().all(|u| under(u.path(), dir)))
        {
            let _ = self.widened.set(dir.clone());
        }
    }
}

/// 放宽一级的证据门槛：子页链接少于这个数，种子才算叶子页。
const WIDEN_MAX_CHILDREN: usize = 3;
/// 放宽一级的证据门槛：同级链接至少这么多。
const WIDEN_MIN_SIBLINGS: usize = 3;

/// 放宽一级的证据门槛：种子页去掉链接后的正文至少这么多字母/数字（约 40 个英文词或一段中文）。
const WIDEN_MIN_PROSE: usize = 200;

/// Markdown 里链接以外的字母/数字个数：`[文字](地址)`、`![图](地址)` 整段不算。
pub(super) fn prose_chars(md: &str) -> usize {
    let b = md.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < md.len() {
        if b[i] == b'[' || (b[i] == b'!' && b.get(i + 1) == Some(&b'[')) {
            // 找配对的 `](...)`；找不到就当普通字符
            let open = if b[i] == b'!' { i + 1 } else { i };
            if let Some(close) = md[open..].find("](").map(|k| open + k)
                && let Some(end) = md[close + 2..].find(')').map(|k| close + 2 + k)
                && !md[open..close].contains('\n')
            {
                i = end + 1;
                continue;
            }
        }
        let c = md[i..].chars().next().unwrap_or(' ');
        n += usize::from(c.is_alphanumeric());
        i += c.len_utf8();
    }
    n
}

/// `path` 是不是 `prefix` 本身或它下面的路径（按路径段边界，`/docs/python` 不包含 `/docs/pythonic`）。
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// 目录的最后一段是不是日期：年份（4 位数字）、月或日（1–2 位数字）、英文月份缩写（`Dec`）。
fn is_date_dir(dir: &str) -> bool {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let last = dir.rsplit('/').next().unwrap_or_default();
    (last.bytes().all(|b| b.is_ascii_digit()) && matches!(last.len(), 1 | 2 | 4))
        || MONTHS.contains(&last.to_ascii_lowercase().as_str())
}

/// `/blog/index.html`、`/blog/index.php` 这类目录首页。
fn is_index_of(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| name.starts_with("index.") || name.starts_with("default."))
}

/// 种子路径最后一段是不是页面名（`/blog/post-3`、`/blog/2026/07/21/post/`），而不是文件名
/// （`/docs/index.html`）。只有页面名种子的范围可能放宽：文件名种子本来就取的是所在目录。
/// 结尾带 `/` 的种子也可能是页面（很多博客这样写文章地址）；是不是目录页交给子页链接判断。
fn seed_names_page(start: &Url) -> bool {
    start
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|last| !last.is_empty() && !last.contains('.'))
}
