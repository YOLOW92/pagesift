//! 各部分的收成：每抓一页，记下它有没有带来新的切题内容（自适应停止判断的"有收获"），按子树
//! 或簇分开统计收获率，用置信区间和其余部分比较（多臂老虎机的逐次淘汰）。
//!
//! - 某部分收获率的上界低于其余部分收获率的下界：再抓下去也追不上，枯竭（撞词的陷阱：期刊系统
//!   一期期目录、商店商品页，每页都沾一个查询词，却从不带来新的切题内容）；
//! - 某部分收获率的下界高于其余部分收获率的下界：产出确实好，按簇分散的上限不该把它赶走。
//!
//! 没有按数据调出来的门槛：样本少时区间宽，不轻易下结论；一直颗粒无收的部分，抓得越多区间越窄，
//! 很快就会被淘汰。置信区间是 Wilson 区间（双侧 `1−δ`，见 [`wilson`]）。

use std::collections::HashMap;

use url::Url;

/// 收获率的 Wilson 置信区间 (下界, 上界)；没有样本时是 (0, 1)。
///
/// Wilson 得分区间（Wilson, JASA 22, 1927），双侧水平 `1−δ`：分位数 `z = Φ⁻¹(1−δ/2)`
/// 。
fn wilson(hits: u32, n: u32) -> (f64, f64) {
    pagesift_stat::beta::wilson_z(f64::from(hits), f64::from(n), *Z)
}

/// 双侧分位数 `Φ⁻¹(1−δ/2)`，进程里只算一次。
static Z: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
    pagesift_stat::special::normal_quantile(1.0 - pagesift_stat::Delta::DEFAULT.get() / 2.0)
});

/// 按某种分法（子树或簇）统计的收获。
#[derive(Default)]
pub struct Harvest {
    /// 部分 → (页数, 有收获的页数)。
    parts: HashMap<String, (u32, u32)>,
    total: (u32, u32),
}

impl Harvest {
    /// 记下属于 `part` 的一页，`hit` 说它有没有带来新的切题内容。
    pub fn observe(&mut self, part: String, hit: bool) {
        let p = self.parts.entry(part).or_default();
        p.0 += 1;
        p.1 += u32::from(hit);
        self.total.0 += 1;
        self.total.1 += u32::from(hit);
    }

    /// 这一部分和其余部分各自的 (页数, 有收获的页数)。
    fn split(&self, part: &str) -> ((u32, u32), (u32, u32)) {
        let mine = self.parts.get(part).copied().unwrap_or_default();
        (mine, (self.total.0 - mine.0, self.total.1 - mine.1))
    }

    /// 枯竭：收获率的上界低于其余部分收获率的下界。
    pub fn is_dry(&self, part: &str) -> bool {
        let ((n, h), (rn, rh)) = self.split(part);
        n > 0 && wilson(h, n).1 < wilson(rh, rn).0
    }

    /// 产出证明了好：收获率的下界高于其余部分收获率的下界。两边都用下界：其余部分只有一两页时
    /// （开局时只有种子页），它的点估计是 0 或 1，拿来比没有意义，下界会把样本少的一边自然压低。
    pub fn is_proven(&self, part: &str) -> bool {
        let ((n, h), (rn, rh)) = self.split(part);
        n > 0 && wilson(h, n).0 > wilson(rh, rn).0
    }
}

/// 页面所在的子树：别的主机（子站）整个算一棵；种子主机上按页面的上级目录分，跳过开头的
/// 语言段，最多两段（`/research-highlight/某篇` → `research-highlight`，
/// `/en/education/programmes/x` → `education/programmes`）。只按第一段分的话，整站都在
/// `/en/` 下的站点就成了一棵树，判它枯竭等于停掉整个爬取。`seed_host` 已去掉 `www.`、小写。
pub fn subtree(seed_host: &str, url: &Url) -> String {
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    if host != seed_host {
        return host;
    }
    let mut segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    segs.pop();
    if segs.first().is_some_and(|s| {
        super::url_shape::is_language_code(s) || super::url_shape::is_language_name(s)
    }) {
        segs.remove(0);
    }
    segs.truncate(2);
    format!("{host}/{}", segs.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_part_that_never_pays_off_is_dropped_once_the_evidence_is_clear() {
        let mut h = Harvest::default();
        // 图书馆的正经页面：一半有收获
        for i in 0..10 {
            h.observe("lib".into(), i % 2 == 0);
        }
        // 期刊系统：颗粒无收。几页的时候还说不准，多了就清楚了
        for _ in 0..3 {
            h.observe("journals".into(), false);
        }
        assert!(!h.is_dry("journals"));
        for _ in 0..12 {
            h.observe("journals".into(), false);
        }
        assert!(h.is_dry("journals"));
        assert!(!h.is_dry("lib"));
        assert!(h.is_proven("lib"));
        assert!(!h.is_proven("journals"));
        // 收获率一般、和其余部分差不多的部分不算枯竭
        let mut h = Harvest::default();
        for i in 0..40 {
            h.observe("study".into(), i % 5 == 0);
            h.observe("other".into(), i % 4 == 0);
        }
        assert!(!h.is_dry("study") && !h.is_dry("other"));
    }

    /// δ = 0.05 时和 0.81 的 z = 1.96 的区间在 1e-4 内一致（冷启动等于现值），没有样本时是 (0, 1)。
    #[test]
    fn wilson_uses_two_sided_delta() {
        let old = |h: f64, n: f64| {
            let z: f64 = 1.96;
            let p = h / n;
            let z2 = z * z;
            let c = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
            let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
            ((c - half).max(0.0), (c + half).min(1.0))
        };
        for (h, n) in [(0, 1), (3, 4), (5, 40), (40, 40)] {
            let (a, b) = wilson(h, n);
            let (c, d) = old(f64::from(h), f64::from(n));
            assert!((a - c).abs() < 1e-4 && (b - d).abs() < 1e-4, "{h}/{n}");
        }
        assert_eq!(wilson(0, 0), (0.0, 1.0));
    }

    #[test]
    fn subtrees_follow_parent_directories() {
        let seed = "e.edu";
        assert_eq!(
            subtree(seed, &u("https://journals.e.edu/j1/issue/current")),
            "journals.e.edu"
        );
        assert_eq!(
            subtree(seed, &u("https://www.e.edu/en/research-highlight/story-1")),
            "e.edu/research-highlight"
        );
        assert_eq!(
            subtree(seed, &u("https://e.edu/en/education/programmes/x/y")),
            "e.edu/education/programmes"
        );
        assert_eq!(subtree(seed, &u("https://e.edu/about")), "e.edu/");
    }
}
