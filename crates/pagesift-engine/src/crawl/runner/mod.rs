use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use pagesift_api::ApiError;
use pagesift_api::types::{CrawlSummary, ScrapeOptions};
use pagesift_net::Url;
use pagesift_policy::AdaptiveConcurrencyGate;
use pagesift_policy::RobotsMode;

use super::attachments::{Attachments, is_document_url};
use crate::engine::Scraper;
use crate::focus::{
    Adaptive, BestFirst, Focus, Harvest, LinkSignal, MIN_RELEVANCE, Offer, PageSignal, cluster_key,
    link_contexts, subtree,
};
use crate::frontier::{Learner, Seen, Tiered};
use pagesift_api::types::CrawlTrace;

use super::{Crawl, MAX_WAIT, Retries, candidate, push_warning, seen_hash};

mod focused;

impl Scraper {
    /// 爬取里抓一页：主机在冷却就等冷却结束再抓（不把排队的页面直接判失败），等待期间不占爬取名额。
    /// 失败后要不要重试由调用方按 [`retry_wait`] 决定：重试排进 [`Retries`]，不在这里原地等，
    /// 免得每主机只有一个并发时，一页的退避把整个站点的爬取都挡住（同 Scrapy 把重试请求放回调度器）。
    async fn crawl_attempt(
        &self,
        url: &Url,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        slots: &AdaptiveConcurrencyGate,
    ) -> Result<pagesift_api::types::Document, ApiError> {
        let host = url.host_str().unwrap_or_default();
        if let Err(wait) = self.policy().backoff().check(host)
            && wait <= MAX_WAIT
        {
            tokio::time::sleep(wait).await;
        }
        let queued_at = std::time::Instant::now();
        let _slot = slots.acquire().await;
        let queue_wait = queued_at.elapsed();
        let result = match self.page_runner() {
            Some(r) => r.run("crawl", url.as_str(), opts, mode).await,
            None => Box::pin(self.scrape_audited("crawl", url.as_str(), opts, mode)).await,
        };
        self.observe_crawl(queue_wait, result.is_err());
        result
    }

    /// 广度优先爬取。
    pub(super) async fn crawl_bfs(&self, c: Crawl<'_>, start: Url) -> Result<(), ApiError> {
        let (req, sink, scope, mode) = (c.req, c.sink, c.scope, c.mode);
        let follow_links = req.sitemap != "only";
        let max_depth = req.max_discovery_depth.unwrap_or(usize::MAX);

        // 队列只装得下 limit 页：广度优先、先进先出，超出上限后发现的 URL 本来就轮不到，
        // 不入队也不记进去重表，大站点上内存不会随发现的链接数增长
        let mut seen: HashSet<u64> = HashSet::new();
        // 模板被降级（大多抓到重复内容）的 URL 排在正常 URL 之后
        let mut queue = Tiered::default();
        let mut learner = Learner::default();
        let mut retries = Retries::new(self.policy().backoff().enabled());
        // 在抓的正常层页面数（降级层的 URL 要等它们都结束才出队）
        let mut normal_inflight = 0usize;
        // 每个主机在抓的页面数：到了每主机并发上限，就先抓别的主机
        let mut host_inflight: HashMap<String, usize> = HashMap::new();
        let per_host = self.policy().limiter().config().per_host_concurrency.max(1);
        // 持久化的任务要记下入队的 URL，重启后据此恢复队列和去重表
        let track = sink.tracks_frontier();
        let mut scheduled = 0usize;
        // 汇总里的页数：成功交付的页面（出错的、被 robots 挡的不算），和 0.81 里绑定自己数的页数一致
        let mut delivered = 0usize;
        // sitemap 里只在放宽一级后才在范围内的 URL：种子页的证据表明要放宽时再入队
        let mut deferred: Vec<Url> = Vec::new();
        if let Some(r) = sink.resume() {
            // 从断点继续：已完成的页面计入 limit，入过队的都算见过，没完成的按原顺序重新排队
            let enqueued: Vec<Url> = r
                .enqueued
                .iter()
                .filter_map(|u| Url::parse(u).ok())
                .collect();
            scope.restore_widened(&enqueued);
            for u in &enqueued {
                seen.insert(seen_hash(u, req.ignore_query_parameters));
            }
            for (u, depth) in r.pending {
                if let Ok(u) = Url::parse(&u) {
                    queue.push(&learner, u, depth);
                }
            }
            scheduled = r.done;
        } else {
            seen.insert(seen_hash(&start, req.ignore_query_parameters));
            queue.push(&learner, start.clone(), 0);
            for u in self.initial_sitemap(&c, &start).await {
                let Some(u) = candidate(&u, "", &learner) else {
                    continue;
                };
                if scope.may_widen_to(&u) {
                    deferred.push(u);
                } else if queue.len() < req.limit
                    && scope.allows(&u)
                    && seen.insert(seen_hash(&u, req.ignore_query_parameters))
                {
                    queue.push(&learner, u, 0);
                }
            }
            if track {
                sink.enqueued(queue.iter().map(|(u, d)| (u.to_string(), d)).collect())
                    .await;
            }
        }

        let wants_links = req.scrape_options.wants("links");
        let wants_md = req.scrape_options.wants("markdown");
        // 判断重复内容要看正文
        let (opts, _) = self.crawl_settings(req, &["markdown"]);
        let mut graph_collector = crate::graph::effective_graph_options(&opts).map(|g| {
            crate::graph::crawl_builder::CrawlGraphCollector::new(
                g,
                self.store_ref().cloned(),
                None,
            )
        });
        let concurrency_ceiling = req.max_concurrency.unwrap_or(usize::MAX).max(1);
        let slots = self.crawl_slots();
        let delay = req.delay.map(Duration::from_secs_f64);

        let mut inflight = FuturesUnordered::new();
        sink.total((scheduled + queue.len()).min(req.limit)).await;
        // 有链接因为 limit 没能进队列（停止原因要区分 limit_reached 和 exhausted）
        let mut overflow = false;
        let mut cancelled = false;
        loop {
            if sink.cancelled().await {
                cancelled = true;
                break;
            }
            while inflight.len() < self.concurrency().min(concurrency_ceiling) {
                let pick = loop {
                    if let Some((u, d, a)) = retries.due() {
                        break Some((u, d, a, false));
                    }
                    if scheduled >= req.limit {
                        break None;
                    }
                    let allow_low = normal_inflight == 0 && retries.is_empty();
                    let busy = |h: &str| host_inflight.get(h).is_some_and(|&n| n >= per_host);
                    let Some((mut url, depth, low)) = queue.pop(&learner, allow_low, busy) else {
                        break None;
                    };
                    // 入队之后学到的改写规则：改写后已经见过的就不抓了
                    if learner.rewrite(&mut url)
                        && !seen.insert(seen_hash(&url, req.ignore_query_parameters))
                    {
                        continue;
                    }
                    scheduled += 1;
                    break Some((url, depth, 0, low));
                };
                let Some((url, depth, attempt, low)) = pick else {
                    break;
                };
                // 配额在发请求之前预留；拒绝时整次爬取立即停下，不再发任何页面请求。
                if let Err(e) = sink.reserve_page(url.as_str()).await {
                    sink.error(url.as_str(), e).await;
                    cancelled = true;
                    break;
                }
                normal_inflight += usize::from(!low);
                *host_inflight.entry(frontier_host(&url)).or_default() += 1;
                let opts = opts.clone();
                inflight.push(async move {
                    let result = self.crawl_attempt(&url, &opts, mode, slots).await;
                    (url, depth, attempt, low, result)
                });
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
            }
            let (url, depth, attempt, low, result) = match inflight.next().await {
                Some(done) => done,
                None => match retries.next_due() {
                    Some(t) => {
                        tokio::time::sleep_until(t).await;
                        continue;
                    }
                    None => break,
                },
            };
            normal_inflight -= usize::from(!low);
            if let Some(n) = host_inflight.get_mut(&frontier_host(&url)) {
                *n -= 1;
                if *n == 0 {
                    host_inflight.remove(&frontier_host(&url));
                }
            }
            if retries.offer(&result, &url, depth, attempt, depth == 0 && url == start) {
                continue;
            }
            match result {
                Ok(mut doc) => {
                    let base = doc
                        .metadata
                        .url
                        .as_deref()
                        .and_then(|u| Url::parse(u).ok())
                        .unwrap_or(url.clone());
                    note_duplicate(&mut learner, &url, &mut doc);
                    if let Some(dup_of) = doc
                        .metadata
                        .pagesift
                        .as_ref()
                        .and_then(|p| p.duplicate_of.clone())
                    {
                        crate::graph::enrich_duplicate_provenance(
                            self.store_ref(),
                            &mut doc,
                            &dup_of,
                            "content_duplicate",
                        );
                    }
                    let mut fresh: Vec<(String, usize)> = Vec::new();
                    if depth == 0 && url == start && scope.can_widen() {
                        let links: Vec<Url> = doc
                            .links
                            .iter()
                            .flatten()
                            .filter_map(|l| candidate(&base, l, &learner))
                            .collect();
                        if let Some(w) = scope.widen_from_seed(
                            &start,
                            doc.markdown.as_deref().unwrap_or_default(),
                            &links,
                        ) {
                            push_warning(&mut doc, w);
                            for u in deferred.drain(..) {
                                if scope.allows(&u)
                                    && scheduled + queue.len() < req.limit
                                    && seen.insert(seen_hash(&u, req.ignore_query_parameters))
                                {
                                    if track {
                                        fresh.push((u.to_string(), 0));
                                    }
                                    queue.push(&learner, u, 0);
                                }
                            }
                        }
                    }
                    if follow_links && depth < max_depth {
                        let mut discovered = 0usize;
                        let mut followed = 0usize;
                        for link in doc.links.iter().flatten() {
                            let Some(u) = candidate(&base, link, &learner) else {
                                continue;
                            };
                            discovered += 1;
                            let h = seen_hash(&u, req.ignore_query_parameters);
                            if !scope.allows(&u) || seen.contains(&h) {
                                continue;
                            }
                            if scheduled + queue.len() >= req.limit {
                                // 满了：正常的新 URL 挤掉一个降级的；新 URL 自己也是降级的就不要了
                                overflow = true;
                                if learner.demoted(&u) {
                                    continue;
                                }
                                match queue.evict_low(&learner) {
                                    Some(old) => {
                                        seen.remove(&seen_hash(&old, req.ignore_query_parameters));
                                    }
                                    None => break,
                                }
                            }
                            seen.insert(h);
                            followed += 1;
                            if track {
                                fresh.push((u.to_string(), depth + 1));
                            }
                            queue.push(&learner, u, depth + 1);
                        }
                        // 种子页明明有链接却一条都没跟进：多半是范围/过滤条件把链接全挡了。
                        // 不再静默返回"成功但只有 1 页"。
                        if depth == 0 && discovered > 0 && followed == 0 {
                            push_warning(
                                &mut doc,
                                format!(
                                    "crawl_followed_zero: discovered {discovered} link(s) on the start page but followed none; the start page looks like a leaf page; use --scope parent for its section, --entire-domain for the whole site, or check --include/--exclude"
                                ),
                            );
                        }
                    }
                    if !fresh.is_empty() {
                        sink.enqueued(fresh).await;
                    }
                    if let Some(gc) = graph_collector.as_mut() {
                        gc.ingest_doc(&mut doc, sink).await;
                    }
                    if !wants_links {
                        doc.links = None;
                    }
                    if !wants_md {
                        doc.markdown = None;
                    }
                    delivered += 1;
                    sink.settle_page(url.as_str()).await;
                    sink.page(doc).await;
                }
                Err(e) => {
                    sink.release_page(url.as_str()).await;
                    sink.error(url.as_str(), e).await;
                }
            }
            sink.total((scheduled + queue.len()).min(req.limit)).await;
        }
        // 取消或提前停止时，在途页面的预留在这里退还
        sink.release_pending().await;
        if let Some(gc) = graph_collector.take() {
            gc.finish(sink).await;
        }
        // 所有爬取结束时都报告停止原因（带查询的见 crawl_focused），绑定不用自己推断
        let stop_reason = if cancelled {
            "cancelled"
        } else if scheduled >= req.limit && (overflow || !queue.is_empty()) {
            "limit_reached"
        } else {
            "exhausted"
        };
        sink.summary(CrawlSummary {
            stop_reason: stop_reason.into(),
            coverage: None,
            term_evidence: Default::default(),
            pages: delivered,
        })
        .await;
        Ok(())
    }
}

fn frontier_host(u: &Url) -> String {
    u.host_str().unwrap_or_default().to_string()
}

/// 带查询的爬取里，模板已降级的链接分数乘以它：排到几乎所有正常链接之后。
const DEMOTED_SCORE: f32 = 0.01;
/// 队首分数低于在途页面最高分的这个比例时，等在途页面回来再调度（它们的链接大概率更好）。
const PATIENCE_RATIO: f32 = 0.6;
/// 队列还能放多少：还能调度的页面数，加上还能退还的失败次数（失败退还预算后，
/// 排在后面的链接要还在，否则退还的名额没有页面可用）。
fn room_left(limit: usize, scheduled: usize, refunds: usize) -> usize {
    limit.saturating_sub(scheduled) + limit.saturating_sub(refunds)
}

/// 一次失败记到主机头上的分量：屏障一次就停（[`HostFailures::IMMEDIATE`]），robots 禁止不算
/// （是这一页的事），超时等网络错误每次 1。
fn failure_weight(e: &ApiError) -> u32 {
    match e.code {
        pagesift_api::ErrorCode::AccessBarrier => HostFailures::IMMEDIATE,
        pagesift_api::ErrorCode::RobotsDisallowed => 0,
        _ => 1,
    }
}

/// 主机连续失败多少次后不再调度它排队的页面。
///
/// Wald 序贯概率比检验（A. Wald, Ann. Math. Statist. 16(2), 1945）：零假设"主机活着、这次失败
/// 是瞬时的"（每次尝试以概率 `q` 失败），备择"主机死了"（每次都失败）。还没成功过的主机连续失败
/// `f` 次的似然比是 `1/qᶠ`；两类错误率都取 `δ`时，Wald 的上门槛是
/// `A = (1−δ)/δ`，`1/qᶠ ≥ A` 时判死，即 `f ≥ ln A / ln(1/q)`。
///
/// `q` 是已经成功过的主机上的失败率（它们确定活着），Beta 后验：先验（先验包 v0：0.81 的现值）
/// 是让 0.81 的"两次就停"恰好成立的 `q₀ = √(δ/(1−δ))`，强度一份观测；后验只在和 `q₀` 的差
/// 超过 `z(1−δ)·SE` 时才用，而且只往宽松的方向用：瞬时失败多（限流、偶发 503）的爬取里
/// 要多失败几次才停，不把活着的子站误停；瞬时失败少时仍是两次（误停的代价是整个子站，
/// 远高于多试一次的一个请求，所以不往更严的方向走）。
#[derive(Default)]
struct HostFailures {
    per_host: HashMap<String, u32>,
    /// 活着的主机上的尝试数和失败数。
    attempts: u32,
    failed: u32,
}

impl HostFailures {
    /// 一次就判死的分量（访问屏障：站点明说不让抓）。
    const IMMEDIATE: u32 = u32::MAX / 2;

    fn add(&mut self, host: &str, weight: u32) {
        let n = self.per_host.entry(host.to_string()).or_default();
        *n = n.saturating_add(weight);
    }

    /// 记下已知活着的主机上的一次尝试。
    fn observe_alive(&mut self, failed: bool) {
        self.attempts += 1;
        self.failed += u32::from(failed);
    }

    /// 当前的判死次数。
    fn limit(&self) -> u32 {
        let delta = pagesift_stat::Delta::DEFAULT;
        let d = delta.get();
        let a = (1.0 - d) / d;
        let prior = (d / (1.0 - d)).sqrt();
        let n = f64::from(self.attempts) + 1.0;
        let post = (f64::from(self.failed) + prior) / n;
        let se = (post * (1.0 - post) / n).sqrt();
        // 只往宽松的方向更新：误判一个活着的子站会丢掉它的全部页面，多试一次只多花一个请求
        let q = if self.attempts > 0
            && post > prior
            && pagesift_stat::shrink::significant(prior, post, se, delta)
        {
            post
        } else {
            prior
        };
        // [推导] 浮点误差：先验恰在边界 1/q₀² = A 上，比值算出 2.0000000000000004 时仍取 2
        let f = (a.ln() / (1.0 / q).ln() - 1e-9).ceil();
        f.clamp(1.0, f64::from(Self::IMMEDIATE)) as u32
    }

    fn blocked(&self, u: &Url) -> bool {
        self.per_host
            .get(u.host_str().unwrap_or_default())
            .is_some_and(|&n| n >= self.limit())
    }
}

/// 记下抓到的页面内容；和之前某页全文相同时在元数据里标出那一页，返回 true。
fn note_duplicate(
    learner: &mut Learner,
    url: &Url,
    doc: &mut pagesift_api::types::Document,
) -> bool {
    let md = doc.markdown.as_deref().unwrap_or_default();
    if let Seen::Duplicate(first) = learner.observe(url, md) {
        doc.metadata
            .pagesift
            .get_or_insert_with(Default::default)
            .duplicate_of = Some(first);
        return true;
    }
    false
}

#[cfg(test)]
mod host_failure_tests {
    use super::HostFailures;

    /// 冷启动两次判死（0.81 的现值）；活着的主机很少失败时仍是两次（只放宽）；瞬时失败多时要更多次。
    #[test]
    fn sprt_limit_follows_the_transient_failure_rate() {
        let mut h = HostFailures::default();
        assert_eq!(h.limit(), 2);
        h.observe_alive(true);
        assert_eq!(h.limit(), 2, "one sample is not significant");
        let mut clean = HostFailures::default();
        (0..100).for_each(|_| clean.observe_alive(false));
        assert_eq!(clean.limit(), 2);
        let mut flaky = HostFailures::default();
        (0..100).for_each(|i| flaky.observe_alive(i % 2 == 0));
        assert!(flaky.limit() >= 4, "{}", flaky.limit());
        let mut b = HostFailures::default();
        b.add("x.com", HostFailures::IMMEDIATE);
        assert!(b.blocked(&pagesift_net::Url::parse("https://x.com/a").unwrap()));
    }
}
