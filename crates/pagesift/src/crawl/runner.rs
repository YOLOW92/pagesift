use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use pagesift_api::ApiError;
use pagesift_api::types::{CrawlSummary, ScrapeOptions};
use pagesift_net::Url;
use pagesift_policy::RobotsMode;
use pagesift_policy::adaptive::AdaptiveConcurrencyGate;

use crate::engine::Scraper;
use crate::focus::{Adaptive, BestFirst, Focus, Offer, link_contexts};
use crate::frontier::{Learner, Seen, Tiered};

use super::{Crawl, MAX_WAIT, Retries, candidate, push_warning, seen_hash};

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
        let result = Box::pin(self.scrape_with(url.as_str(), opts, mode)).await;
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
        let mut retries = Retries::default();
        // 在抓的正常层页面数（降级层的 URL 要等它们都结束才出队）
        let mut normal_inflight = 0usize;
        // 每个主机在抓的页面数：到了每主机并发上限，就先抓别的主机
        let mut host_inflight: HashMap<String, usize> = HashMap::new();
        let per_host = self.policy().limiter().config().per_host_concurrency.max(1);
        // 持久化的任务要记下入队的 URL，重启后据此恢复队列和去重表
        let track = sink.tracks_frontier();
        let mut scheduled = 0usize;
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
        let concurrency_ceiling = req.max_concurrency.unwrap_or(usize::MAX).max(1);
        let slots = self.crawl_slots();
        let delay = req.delay.map(Duration::from_secs_f64);

        let mut inflight = FuturesUnordered::new();
        sink.total((scheduled + queue.len()).min(req.limit)).await;
        loop {
            if sink.cancelled().await {
                return Ok(());
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
            if retries.offer(&result, &url, depth, attempt) {
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
                    if !wants_links {
                        doc.links = None;
                    }
                    if !wants_md {
                        doc.markdown = None;
                    }
                    sink.page(doc).await;
                }
                Err(e) => sink.error(url.as_str(), e).await,
            }
            sink.total((scheduled + queue.len()).min(req.limit)).await;
        }
        Ok(())
    }

    /// 相关性优先爬取：队列按链接分数出队；`adaptive` 时信息够了就停止调度，
    /// 已经在抓的页面照常交付。结束时通过 [`CrawlSink::summary`] 报告停止原因。
    ///
    /// 从断点继续时队列和去重表照常恢复，但链接分数和自适应的统计不落盘：没完成的页面按 URL 路径
    /// 重新打分，覆盖度从零开始累计。
    pub(super) async fn crawl_focused(
        &self,
        c: Crawl<'_>,
        start: Url,
        query: &str,
    ) -> Result<(), ApiError> {
        let (req, sink, scope, mode) = (c.req, c.sink, c.scope, c.mode);
        let focus = Focus::new(query);
        let mut adaptive = req
            .adaptive
            .clone()
            .map(|a| Adaptive::new(a, focus.query.len()));
        let follow_links = req.sitemap != "only";
        let max_depth = req.max_discovery_depth.unwrap_or(usize::MAX);
        let ignore_query = req.ignore_query_parameters;

        // 去重表只记已调度和正在排队的页面；被挤出队列的链接从表里删掉，以后还能以更高的分数回来。
        // 两者加起来不超过 limit，和广度优先一样有界
        let mut seen: HashSet<u64> = HashSet::new();
        let mut queue = BestFirst::default();
        let mut learner = Learner::default();
        let mut retries = Retries::default();
        // 有页面因为 limit 没能进队列
        let mut overflow = false;
        let track = sink.tracks_frontier();
        let mut scheduled = 0usize;
        // 新入队的 URL（持久化的任务要回报）
        let mut fresh: Vec<(String, usize)> = Vec::new();
        // 放进队列，维护去重表；返回有没有页面因为 limit 被挤掉或进不来
        let offer = |queue: &mut BestFirst,
                     seen: &mut HashSet<u64>,
                     fresh: &mut Vec<(String, usize)>,
                     u: Url,
                     depth: usize,
                     score: f32,
                     room: usize|
         -> bool {
            let h = seen_hash(&u, ignore_query);
            let entry = track.then(|| (u.to_string(), depth));
            match queue.offer(h, u, depth, score, room) {
                Offer::Queued => {
                    seen.insert(h);
                    fresh.extend(entry);
                    false
                }
                Offer::Evicted(old) => {
                    seen.remove(&old);
                    seen.insert(h);
                    fresh.extend(entry);
                    true
                }
                Offer::Rejected => true,
                Offer::AlreadyQueued => false,
            }
        };
        // 同广度优先：只在放宽一级后才在范围内的 sitemap URL 先留着
        let mut deferred: Vec<Url> = Vec::new();
        if let Some(r) = sink.resume() {
            let enqueued: Vec<Url> = r
                .enqueued
                .iter()
                .filter_map(|u| Url::parse(u).ok())
                .collect();
            scope.restore_widened(&enqueued);
            for u in &enqueued {
                seen.insert(seen_hash(u, ignore_query));
            }
            scheduled = r.done;
            let room = req.limit.saturating_sub(scheduled);
            // 这些 URL 已经记在入队列表里，不再回报
            let mut known = Vec::new();
            for (u, depth) in r.pending {
                if let Ok(u) = Url::parse(&u) {
                    let score = focus.link_score(&u, None, None, 0.0, depth);
                    overflow |= offer(&mut queue, &mut seen, &mut known, u, depth, score, room);
                }
            }
        } else {
            offer(
                &mut queue,
                &mut seen,
                &mut fresh,
                start.clone(),
                0,
                f32::INFINITY,
                req.limit,
            );
            for u in self.initial_sitemap(&c, &start).await {
                let Some(u) = candidate(&u, "", &learner) else {
                    continue;
                };
                if scope.may_widen_to(&u) {
                    deferred.push(u);
                    continue;
                }
                if !scope.allows(&u) || seen.contains(&seen_hash(&u, ignore_query)) {
                    continue;
                }
                let score = focus.link_score(&u, None, None, 0.0, 0);
                overflow |= offer(&mut queue, &mut seen, &mut fresh, u, 0, score, req.limit);
            }
        }
        if !fresh.is_empty() {
            sink.enqueued(std::mem::take(&mut fresh)).await;
        }

        let wants_links = req.scrape_options.wants("links");
        let wants_md = req.scrape_options.wants("markdown");
        let (opts, _) = self.crawl_settings(req, &["markdown"]);
        let concurrency_ceiling = req.max_concurrency.unwrap_or(usize::MAX).max(1);
        let slots = self.crawl_slots();
        let delay = req.delay.map(Duration::from_secs_f64);

        let mut stop: Option<&'static str> = None;
        let mut inflight = FuturesUnordered::new();
        sink.total((scheduled + queue.len()).min(req.limit)).await;
        loop {
            if sink.cancelled().await {
                stop = Some("cancelled");
                break;
            }
            while stop.is_none() && inflight.len() < self.concurrency().min(concurrency_ceiling) {
                let pick = match retries.due() {
                    Some(r) => Some(r),
                    None if scheduled < req.limit => queue.pop().map(|(u, d, _)| {
                        scheduled += 1;
                        (u, d, 0)
                    }),
                    None => None,
                };
                let Some((url, depth, attempt)) = pick else {
                    break;
                };
                let opts = opts.clone();
                inflight.push(async move {
                    let result = self.crawl_attempt(&url, &opts, mode, slots).await;
                    (url, depth, attempt, result)
                });
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
            }
            let (url, depth, attempt, result) = match inflight.next().await {
                Some(done) => done,
                None => match retries.next_due().filter(|_| stop.is_none()) {
                    Some(t) => {
                        tokio::time::sleep_until(t).await;
                        continue;
                    }
                    None => break,
                },
            };
            if stop.is_none() && retries.offer(&result, &url, depth, attempt) {
                continue;
            }
            match result {
                Ok(mut doc) => {
                    note_duplicate(&mut learner, &url, &mut doc);
                    let md = doc.markdown.as_deref().unwrap_or_default();
                    let relevance = focus.page_relevance(doc.metadata.title.as_deref(), md);
                    if stop.is_none()
                        && let Some(a) = adaptive.as_mut()
                    {
                        stop = a.observe(&focus.query, md);
                    }
                    let base = doc
                        .metadata
                        .url
                        .as_deref()
                        .and_then(|u| Url::parse(u).ok())
                        .unwrap_or(url.clone());
                    // 正文还借给链接打分用，警告等链接处理完再写
                    let mut scope_note = None;
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
                            scope_note = Some(w);
                            let room = req.limit - scheduled;
                            for u in deferred.drain(..) {
                                if !scope.allows(&u) || seen.contains(&seen_hash(&u, ignore_query))
                                {
                                    continue;
                                }
                                let score = focus.link_score(&u, None, None, 0.0, 0);
                                overflow |=
                                    offer(&mut queue, &mut seen, &mut fresh, u, 0, score, room);
                            }
                        }
                    }
                    if stop.is_none() && follow_links && depth < max_depth {
                        let contexts = link_contexts(md);
                        for link in doc.links.iter().flatten() {
                            let Some(u) = candidate(&base, link, &learner) else {
                                continue;
                            };
                            let h = seen_hash(&u, ignore_query);
                            // 已经调度过的页面不再入队；还在排队的可以按更高的分数前移
                            if !scope.allows(&u) || (seen.contains(&h) && !queue.contains(h)) {
                                continue;
                            }
                            let ctx = contexts.get(link.split('#').next().unwrap_or(link));
                            let mut score = focus.link_score(
                                &u,
                                ctx.map(|c| c.anchor.as_str()),
                                ctx.map(|c| c.line.as_str()),
                                relevance,
                                depth + 1,
                            );
                            if learner.demoted(&u) {
                                score *= DEMOTED_SCORE;
                            }
                            let room = req.limit - scheduled;
                            overflow |=
                                offer(&mut queue, &mut seen, &mut fresh, u, depth + 1, score, room);
                        }
                    }
                    if !fresh.is_empty() {
                        sink.enqueued(std::mem::take(&mut fresh)).await;
                    }
                    if let Some(w) = scope_note {
                        push_warning(&mut doc, w);
                    }
                    if !wants_links {
                        doc.links = None;
                    }
                    if !wants_md {
                        doc.markdown = None;
                    }
                    doc.metadata
                        .pagesift
                        .get_or_insert_with(Default::default)
                        .relevance = Some((relevance * 1000.0).round() / 1000.0);
                    sink.page(doc).await;
                }
                Err(e) => sink.error(url.as_str(), e).await,
            }
            let pending = if stop.is_some() { 0 } else { queue.len() };
            sink.total((scheduled + pending).min(req.limit)).await;
        }
        let stop_reason = stop.unwrap_or(
            if scheduled >= req.limit && (overflow || !queue.is_empty()) {
                "limit_reached"
            } else {
                "exhausted"
            },
        );
        let term_evidence = match &adaptive {
            Some(a) => focus
                .query
                .terms()
                .iter()
                .cloned()
                .zip(a.evidence().iter().copied())
                .collect(),
            None => Default::default(),
        };
        sink.summary(CrawlSummary {
            stop_reason: stop_reason.into(),
            coverage: adaptive
                .as_ref()
                .map(|a| (a.coverage() * 1000.0).round() / 1000.0),
            term_evidence,
            pages: scheduled,
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

/// 记下抓到的页面内容；和之前某页全文相同时在元数据里标出那一页。
fn note_duplicate(learner: &mut Learner, url: &Url, doc: &mut pagesift_api::types::Document) {
    let md = doc.markdown.as_deref().unwrap_or_default();
    if let Seen::Duplicate(first) = learner.observe(url, md) {
        doc.metadata
            .pagesift
            .get_or_insert_with(Default::default)
            .duplicate_of = Some(first);
    }
}
