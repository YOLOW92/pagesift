//! 渲染决策与候选合并的引擎接线：学习表和族的键在 `pagesift_plan::render`，这里把它接到请求路径上
//! （渲不渲染、换不换内核、保留哪个结果、页面数据岛），并负责把学习表存进存储。

pub use pagesift_plan::render::*;

use pagesift_api::types::PlanStep;
use pagesift_plan::merge::{Candidate, Quality, merge};
use pagesift_stat::Delta;

/// 存储里的键。
const STORE_KEY: &[u8] = b"plan/render";

/// 学习表写进存储的 KV。
pub fn save(planner: &RenderPlanner, store: &pagesift_store::Store) -> pagesift_store::Result<()> {
    store.tasks().kv_put(STORE_KEY, &planner.encode())
}

/// 从存储读回学习表；没有时保持空表（冷启动）。
pub fn load(planner: &RenderPlanner, store: &pagesift_store::Store) {
    if let Ok(Some(buf)) = store.tasks().kv_get(STORE_KEY) {
        planner.decode_merge(&buf);
    }
}

/// `embedded-json` 候选：页面数据岛（`__NEXT_DATA__`、ld+json 等）里的正文与 HTTP 结果按块合并，
/// 合并结果补进了数据岛独有的块、且质量分高于 HTTP 结果时，把 `doc.markdown` 换成合并结果。
/// 没有数据岛或质量模型时返回 `None`，否则返回这一步的依据。
pub fn embedded_json(doc: &mut pagesift_content::Document, html: &str) -> Option<PlanStep> {
    let blocks = pagesift_plan::embedded_json::extract(html, Delta::DEFAULT);
    if blocks.is_empty() {
        return None;
    }
    let qh = quality(doc, html.len())?;
    // 换进候选 Markdown 算质量分，再换回来：不复制整个文档
    let http_md = std::mem::replace(&mut doc.markdown, pagesift_plan::block::join(&blocks));
    let qj = quality(doc, html.len());
    let json_md = std::mem::replace(&mut doc.markdown, http_md);
    let qj = qj?;
    let (md, from) = merge_markdown(&doc.markdown, qh, &json_md, qj);
    let mut step = PlanStep {
        action: "embedded-json".into(),
        learned: true,
        blocks: Some(from),
        quality: Some(qh),
        ..PlanStep::default()
    };
    // from[2]：只来自数据岛的块数
    if from[2] == 0 {
        return Some(step);
    }
    let http_md = std::mem::replace(&mut doc.markdown, md);
    let qm = quality(doc, html.len()).unwrap_or(qh);
    step.value = Some(qm - qh);
    if qm > qh {
        step.chosen = true;
        step.quality = Some(qm);
        doc.plain_text = None;
    } else {
        doc.markdown = http_md;
    }
    Some(step)
}

/// HTTP 与渲染两个候选按块合并。返回合并后的 Markdown 和各块来源计数（两边都有、只来自 HTTP、只来自渲染）。
/// [推导] 计数数组长度 3 = 来源种类数（`pagesift_plan::merge::From` 的变体数）
/// 质量区间暂取点估计（模型还没有给出单页区间），所以"区间不劣于"退化为"不低于"。
pub fn merge_markdown(
    http: &str,
    http_quality: f64,
    rendered: &str,
    rendered_quality: f64,
) -> (String, [usize; 3]) {
    let point = |q: f64| Quality {
        mean: q,
        lo: q,
        hi: q,
    };
    let m = merge(
        &Candidate {
            blocks: pagesift_plan::block::split(http),
            quality: point(http_quality),
        },
        &Candidate {
            blocks: pagesift_plan::block::split(rendered),
            quality: point(rendered_quality),
        },
        Delta::default(),
    );
    let mut from = [0; 3]; // [推导] 来源种类数，同上
    for f in &m.from {
        from[match f {
            pagesift_plan::merge::From::Both => 0,
            pagesift_plan::merge::From::Left => 1,
            pagesift_plan::merge::From::Right => 2,
        }] += 1;
    }
    (m.markdown(), from)
}

impl super::Scraper {
    /// HTTP 结果已经出来、0.81 规则认为值得渲染时，按学到的后验决定是否真的渲染。
    /// 浏览器的影子价格暂取 0（空闲时的值）：浏览器池的利用率接口在 0.86 改完后接上。
    /// 返回决定和它的依据（`metadata.pagesift.plan` 的一步）。
    pub(super) fn plan_render(
        &self,
        url: &str,
        http: &pagesift_content::Document,
        html_bytes: usize,
        hints: &super::RenderHints,
        opts: &pagesift_api::types::ScrapeOptions,
        started: std::time::Instant,
    ) -> (bool, PlanStep) {
        let q = quality(http, html_bytes);
        let fallback = self.render_stats.default_timeout();
        let remaining = super::render::request_deadline(opts, started).map_or(fallback, |d| {
            d.saturating_duration_since(std::time::Instant::now())
        });
        let render_secs = self.render_stats.budget().unwrap_or(fallback).as_secs_f64();
        let family = render_family(url, hints.client_code);
        let (go, why) =
            self.planner
                .decide(&family, true, render_secs, remaining.as_secs_f64(), 0.0);
        let step = PlanStep {
            action: "render".into(),
            chosen: go,
            learned: why.learned,
            p: Some(why.p),
            value: Some(why.value),
            charge: Some(why.charge),
            quality: q,
            blocks: None,
        };
        (go, step)
    }

    /// 当前内核渲染出的正文太少（0.81 的规则，`thin`）时，是否换下一个内核再试：
    /// 换 ⇔ `P(下一个内核更好 | 族) > E[渲染秒] · (λ_browser + 1/剩余期限)`（同 `RenderPlanner::decide`），
    /// 族里还没有数据时照 0.81 的规则换。结果出来后用 [`Self::record_fallback`] 记下有没有变好。
    pub(super) fn plan_fallback(
        &self,
        family: &str,
        thin: bool,
        current_quality: Option<f64>,
        opts: &pagesift_api::types::ScrapeOptions,
        started: std::time::Instant,
    ) -> (bool, PlanStep) {
        if !thin {
            return (
                false,
                PlanStep {
                    action: "fallback".into(),
                    quality: current_quality,
                    ..PlanStep::default()
                },
            );
        }
        let fallback = self.render_stats.default_timeout();
        let remaining = super::render::request_deadline(opts, started).map_or(fallback, |d| {
            d.saturating_duration_since(std::time::Instant::now())
        });
        let render_secs = self.render_stats.budget().unwrap_or(fallback).as_secs_f64();
        let (go, why) =
            self.planner
                .decide(family, true, render_secs, remaining.as_secs_f64(), 0.0);
        let step = PlanStep {
            action: "fallback".into(),
            chosen: go,
            learned: why.learned,
            p: Some(why.p),
            value: Some(why.value),
            charge: Some(why.charge),
            quality: current_quality,
            blocks: None,
        };
        (go, step)
    }

    /// 换内核之后记下有没有变好：下一个内核的结果字更多，或者质量分更高（同 [`Self::adopt_render`]：
    /// 质量分不看篇幅，只能多算增益）。`before`、`after` 是 (质量分, 加权字数)。
    pub(super) fn record_fallback(
        &self,
        family: &str,
        before: (Option<f64>, usize),
        after: (Option<f64>, usize),
    ) {
        let better_quality = matches!((before.0, after.0), (Some(b), Some(a)) if a > b);
        self.planner
            .record(family, after.1 > before.1 || better_quality);
    }

    /// 渲染回来后决定用哪个结果，并把"渲染有没有增益"记进 planner。
    /// 三个候选：HTTP、渲染、两者按块合并；合并结果的质量分不低于渲染结果、且确实补进了 HTTP 独有的块时用合并结果。
    /// 返回 `None` 表示留 HTTP 的结果。质量分算不出来（内置模型没加载）时退回 0.81 的"渲染出的字更多"。
    /// 第二项是这一步的依据（`adopt`：是否采用渲染或合并的结果；`blocks` 是合并时各块的来源）。
    pub(super) fn adopt_render(
        &self,
        url: &str,
        http: &pagesift_content::Document,
        html_bytes: usize,
        client_code: bool,
        http_chars: usize,
        mut rendered: pagesift_api::types::Document,
    ) -> (Option<pagesift_api::types::Document>, PlanStep) {
        let mut step = PlanStep {
            action: "adopt".into(),
            ..PlanStep::default()
        };
        let (Some(qh), Some(qr)) = (quality(http, html_bytes), rendered.quality) else {
            let more =
                super::text_weight(rendered.markdown.as_deref().unwrap_or_default()) > http_chars;
            step.chosen = more;
            return (more.then_some(rendered), step);
        };
        let mut best = qr;
        let mut merged_md = None;
        if let Some(rmd) = rendered.markdown.as_deref() {
            let (md, from) = merge_markdown(&http.markdown, qh, rmd, qr);
            step.blocks = Some(from);
            // from[1]：只来自 HTTP 的块数
            if from[1] > 0 {
                let mut m = http.clone();
                m.markdown = md;
                m.plain_text = None;
                if let Some(qm) = quality(&m, html_bytes)
                    && qm >= qr
                {
                    best = qm;
                    merged_md = Some(m.markdown);
                }
            }
        }
        // 质量分只能增加采用、不能否决 0.81 会采用的结果（渲染出的字更多）：质量分不看篇幅，
        // HTTP 只拿到一小段时分数也可能不低
        let more =
            super::text_weight(rendered.markdown.as_deref().unwrap_or_default()) > http_chars;
        let gain = more || best > qh;
        self.planner.record(&render_family(url, client_code), gain);
        step.chosen = gain;
        step.learned = true;
        step.value = Some(best - qh);
        step.quality = Some(if gain { best.max(qh) } else { qh });
        if !gain {
            return (None, step);
        }
        if let Some(md) = merged_md {
            rendered.markdown = Some(md);
            step.action = "merge".into();
        }
        (Some(rendered), step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_follows_the_heuristic_then_learns() {
        let p = RenderPlanner::default();
        let url = &family("https://example.com/docs/a");
        assert!(p.decide(url, true, 1.0, 30.0, 0.0).0);
        assert!(!p.decide(url, false, 1.0, 30.0, 0.0).0);
        // 同族多次渲染都没有增益之后，即使启发式说要渲染，也基本不再渲染
        for _ in 0..200 {
            p.record(&family("https://example.com/docs/b"), false);
        }
        let renders = (0..100)
            .filter(|_| p.decide(url, true, 1.0, 30.0, 0.0).0)
            .count();
        assert!(renders < 10, "{renders}");
        // 别的族不受影响
        assert!(
            p.decide(&family("https://example.com/app/x"), true, 1.0, 30.0, 0.0)
                .0
        );
        assert_eq!(
            family("https://example.com/docs/a?x=1"),
            "https://example.com/docs"
        );
    }

    /// 0.90 用 `p·(1 − Q_http)` 比较代价：HTTP 结果质量分 0.97（基本率 0.951 附近的常见值）时
    /// 即使这一族每次渲染都有增益也不再渲染。现在只看 `p` 与代价。
    #[test]
    fn a_family_where_rendering_always_helps_keeps_rendering() {
        let p = RenderPlanner::default();
        let url = &family("https://example.com/spa/a");
        for _ in 0..50 {
            p.record(url, true);
        }
        let renders = (0..100)
            .filter(|_| p.decide(url, true, 1.0, 30.0, 0.0).0)
            .count();
        assert!(renders > 90, "{renders}");
        let (_, why) = p.decide(url, true, 1.0, 30.0, 0.0);
        assert!(why.learned);
        assert_eq!(why.value, 1.0);
    }

    #[test]
    fn merging_fills_in_blocks_the_http_result_missed() {
        let http = "# T\n\nIntro paragraph here.\n";
        let rendered = "# T\n\nIntro paragraph here.\n\nLoaded by script.\n";
        let (md, from) = merge_markdown(http, 0.4, rendered, 0.8);
        assert_eq!(md, rendered);
        assert_eq!(from, [2, 0, 1]);
    }

    #[test]
    fn embedded_json_fills_in_a_script_rendered_article() {
        let body = "The council approved the new transit plan on Tuesday after a long debate. \
                    Supporters said the plan would cut commute times across the city by a third. \
                    Opponents argued the budget estimates were optimistic and asked for an audit. \
                    The first new lines are expected to open in two years, officials said.";
        let html = format!(
            "<html><head><script type=\"application/ld+json\">{{\"@type\":\"NewsArticle\",\
             \"headline\":\"Transit plan\",\"articleBody\":\"{body}\"}}</script></head>\
             <body><div id=app></div><script src=app.js></script></body></html>"
        );
        let opts = pagesift_content::Options::default();
        let mut doc = pagesift_content::process(&html, &opts);
        let before = doc.markdown.clone();
        // 内置质量模型随二进制发布（`pagesift_plan::quality` 的测试保证它能解码），这里必须出候选：
        // 0.90 的这个测试在取不到候选时直接 return，掩盖了小数据岛总是取空的问题
        assert!(pagesift_plan::quality::QualityModel::builtin().is_some());
        let step = embedded_json(&mut doc, &html).expect("a candidate from the JSON-LD island");
        assert_eq!(step.action, "embedded-json");
        // HTTP 只拿到空壳，数据岛补进整篇正文：质量分应当更高
        assert!(step.chosen, "{step:?}");
        assert!(doc.markdown.contains("transit plan"), "{}", doc.markdown);
        assert_ne!(doc.markdown, before);
        // 没有数据岛的页面不出候选
        let mut plain = pagesift_content::process("<p>hello</p>", &opts);
        assert!(embedded_json(&mut plain, "<p>hello</p>").is_none());
    }
}
