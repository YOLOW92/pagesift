//! 一次爬取里学到的东西：按模板的重复内容降级，以及从重复页面对学到的 URL 改写规则。

use super::*;

/// 一条改写规则要被几对不同的页面证实才生效。
const RULE_SUPPORT: usize = 2;

/// 模板至少抓了几页才判断，重复页占多少比例时降级。
const DEMOTE_MIN_FETCHED: u32 = 4;
const DEMOTE_SHARE: f32 = 0.75;

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
    pub(super) generation: u64,
}

impl ShapeStats {
    fn demoted(&self) -> bool {
        self.fetched >= DEMOTE_MIN_FETCHED
            && self.repeated as f32 >= DEMOTE_SHARE * self.fetched as f32
    }
}

pub(super) fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
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

    pub(super) fn demoted_key(&self, key: u64) -> bool {
        self.shapes.get(&key).is_some_and(ShapeStats::demoted)
    }
}
