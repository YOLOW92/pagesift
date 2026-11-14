//! 带查询时的相关性优先爬取（见 [`crate::focus`]）。

use super::*;

impl Scraper {
    /// 相关性优先爬取：队列按链接分数出队；`adaptive` 时信息够了就停止调度，
    /// 已经在抓的页面照常交付。结束时通过 [`CrawlSink::summary`] 报告停止原因。
    ///
    /// 从断点继续时队列和去重表照常恢复，但链接分数和自适应的统计不落盘：没完成的页面按 URL 路径
    /// 重新打分，覆盖度从零开始累计。
    pub(in crate::crawl) async fn crawl_focused(
        &self,
        c: Crawl<'_>,
        start: Url,
        query: &str,
    ) -> Result<(), ApiError> {
        let (req, sink, scope, mode) = (c.req, c.sink, c.scope, c.mode);
        let mut focus = Focus::new(query).with_host(start.host_str().unwrap_or_default());
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
        let mut queue: BestFirst<LinkSignal> = BestFirst::default();
        let mut learner = Learner::default();
        let mut retries = Retries::new(self.policy().backoff().enabled());
        // 有页面因为 limit 没能进队列
        let mut overflow = false;
        let track = sink.tracks_frontier();
        let mut scheduled = 0usize;
        // 每个簇（主机 + 第一段路径）已调度的页数：一个子树最多占一半预算，除非别处没有像样的候选
        let mut clusters: HashMap<String, usize> = HashMap::new();
        let cluster_cap = (req.limit / 3).max(3);
        // 自适应时每抓一页记下它有没有带来新的切题内容，按簇和按子树统计（见 focus::harvest）：
        // 收获率明显高于别处的簇不受上限限制；明显追不上别处的子树（撞词的陷阱）不再往里抓
        let mut by_cluster = Harvest::default();
        let mut by_subtree = Harvest::default();
        let seed_host = start
            .host_str()
            .unwrap_or_default()
            .trim_start_matches("www.")
            .to_ascii_lowercase();
        // 在途页面的分数：队首比在途的最好页面差得多时先等它们回来再调度，
        // 否则首页一返回就把并发槽位全填上新闻页，真正相关的子页还没发现预算已经用掉了
        let mut inflight_scores: HashMap<String, f32> = HashMap::new();
        // 已调度页面入队时的打分依据：页面回来后按它是否相关学习链接权重（focus::model）
        let mut signals: HashMap<String, LinkSignal> = HashMap::new();
        // 每个排队链接的父页（按去重哈希），出队时转成来历记录
        let mut parents: HashMap<u64, String> = HashMap::new();
        // 自适应停止：连续没有新信息的页面里分数最高的（见下面的"不算饱和"规则）
        let mut stale_max: f32 = 0.0;
        // 抓取失败（网络错误、屏障、错误状态页、robots 禁止）不占页面预算，最多退还 limit 次：
        // 带查询的爬取里 `limit` 数的是交付的页面
        let mut refunds = 0usize;
        // 主机的失败次数：屏障一次、超时等网络错误两次之后，这个主机上排队的页面都不再调度。
        // 已经正常交付过页面的主机不算（大站点的某一页被防火墙拦了，不代表整个主机都不通）
        let mut host_failures = HostFailures::default();
        let mut host_ok: HashSet<String> = HashSet::new();
        // 已经交付的页面（按跳转后的地址）：两个地址跳到同一页时只交付一次
        let mut delivered: HashSet<u64> = HashSet::new();
        // 已调度页面的来历：深度、父页、分数、是不是附件
        let mut traces: HashMap<String, CrawlTrace> = HashMap::new();
        let mut attachments = req
            .attachments
            .then(|| Attachments::new(req.attachment_limit.unwrap_or(req.limit)));
        let start_site = pagesift_content::site_key(start.host_str().unwrap_or_default());
        // 新入队的 URL（持久化的任务要回报）
        let mut fresh: Vec<(String, usize)> = Vec::new();
        // 放进队列，维护去重表；返回有没有页面因为 limit 被挤掉或进不来
        let offer = |queue: &mut BestFirst<LinkSignal>,
                     seen: &mut HashSet<u64>,
                     fresh: &mut Vec<(String, usize)>,
                     u: Url,
                     depth: usize,
                     (score, signal): (f32, LinkSignal),
                     room: usize|
         -> bool {
            let h = seen_hash(&u, ignore_query);
            let entry = track.then(|| (u.to_string(), depth));
            match queue.offer(h, u, depth, score, signal, room) {
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
                    let s = focus.link_signal(&u, None, None, PageSignal::default(), depth);
                    let scored = (focus.score(&s), s);
                    overflow |= offer(&mut queue, &mut seen, &mut known, u, depth, scored, room);
                }
            }
        } else {
            offer(
                &mut queue,
                &mut seen,
                &mut fresh,
                start.clone(),
                0,
                (f32::INFINITY, LinkSignal::fixed(f32::INFINITY)),
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
                let s = focus.link_signal(&u, None, None, PageSignal::default(), 0);
                let scored = (focus.score(&s), s);
                overflow |= offer(&mut queue, &mut seen, &mut fresh, u, 0, scored, req.limit);
            }
        }
        if !fresh.is_empty() {
            sink.enqueued(std::mem::take(&mut fresh)).await;
        }

        let wants_links = req.scrape_options.wants("links");
        let wants_md = req.scrape_options.wants("markdown");
        let (opts, _) = self.crawl_settings(req, &["markdown"]);
        let graph_versions =
            crate::graph::effective_graph_options(&opts).is_some_and(|g| g.versions);
        let mut graph_collector = crate::graph::effective_graph_options(&opts).map(|g| {
            crate::graph::crawl_builder::CrawlGraphCollector::new(
                g,
                self.store_ref().cloned(),
                None,
                opts.owner.clone(),
            )
        });
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
            // 自适应停止之后不再调度页面，但相关页面上已经发现的附件照抓
            while inflight.len() < self.concurrency().min(concurrency_ceiling) {
                let pick = match (stop.is_none(), retries.due()) {
                    (false, _) => None,
                    (true, Some(r)) => Some(r),
                    (true, None) if scheduled < req.limit => {
                        let best_inflight = inflight_scores.values().copied().fold(0.0, f32::max);
                        let head = queue.peek_score().unwrap_or(0.0);
                        if !inflight_scores.is_empty() && head < PATIENCE_RATIO * best_inflight {
                            break;
                        }
                        // 队首只是盲探（链接本身没有查询的线索）时一页一页地探：一个展品列表、
                        // 新闻列表回来，几十条盲探链接一次全派出去，停止的判断还没来得及做出来
                        // 它们就都在途了；在途的页面回来也往往带来有线索的链接
                        if !inflight_scores.is_empty() && crate::focus::is_blind(head) {
                            break;
                        }
                        // 还没有任何收获时一页一页地抓：每一页都可能改变后面的判断（英文首页回来
                        // 以后日文栏目不再领保底分；栏目页带来有线索的链接，别的栏目就不用盲探），
                        // 同时派出去的几页是在知道这些之前选的。一有收获就恢复并发
                        if !inflight_scores.is_empty()
                            && !adaptive.as_ref().is_some_and(|a| a.has_found())
                        {
                            break;
                        }
                        let mut next =
                            queue.pop_diverse(&clusters, cluster_cap, |k| by_cluster.is_proven(k));
                        while let Some((u, _, _)) = &mut next {
                            if host_failures.blocked(u)
                                || by_subtree.is_dry(&subtree(&seed_host, u))
                            {
                                next = queue.pop_diverse(&clusters, cluster_cap, |k| {
                                    by_cluster.is_proven(k)
                                });
                                continue;
                            }
                            // 入队之后学到的改写规则（`?ref=` 这类不改内容的参数）：
                            // 改写后已经见过的就不抓了；父页记录跟着改写后的地址走
                            let before = seen_hash(u, ignore_query);
                            if learner.rewrite(u) {
                                let after = seen_hash(u, ignore_query);
                                let parent = parents.remove(&before);
                                if !seen.insert(after) {
                                    next = queue.pop_diverse(&clusters, cluster_cap, |k| {
                                        by_cluster.is_proven(k)
                                    });
                                    continue;
                                }
                                parents.extend(parent.map(|p| (after, p)));
                            }
                            break;
                        }
                        let signal = queue.take_signal();
                        next.map(|(u, d, s)| {
                            if let Some(sig) = signal {
                                signals.insert(u.to_string(), sig);
                            }
                            scheduled += 1;
                            *clusters.entry(cluster_key(&u)).or_default() += 1;
                            inflight_scores.insert(u.to_string(), s);
                            traces.insert(
                                u.to_string(),
                                CrawlTrace {
                                    depth: d,
                                    parent: parents.remove(&seen_hash(&u, ignore_query)),
                                    score: s.is_finite().then_some(s),
                                    attachment_of: None,
                                },
                            );
                            (u, d, 0)
                        })
                    }
                    (true, None) => None,
                };
                // 页面预算用完或暂时没有页面可调度时，抓排队的附件
                let pick = pick.or_else(|| {
                    let (u, d, s) = attachments.as_mut()?.next()?;
                    let parent = parents.remove(&seen_hash(&u, ignore_query));
                    traces.insert(
                        u.to_string(),
                        CrawlTrace {
                            depth: d,
                            parent: parent.clone(),
                            score: Some(s),
                            attachment_of: parent,
                        },
                    );
                    Some((u, d, 0))
                });
                let Some((url, depth, attempt)) = pick else {
                    break;
                };
                // 配额在发请求之前预留；拒绝时停止调度（在途页面照常收尾）。
                if let Err(e) = sink.reserve_page(url.as_str()).await {
                    sink.error(url.as_str(), e).await;
                    stop = Some("cancelled");
                    break;
                }
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
            inflight_scores.remove(url.as_str());
            let learned_from = signals.remove(url.as_str());
            let seed = depth == 0 && url == start;
            if host_ok.contains(url.host_str().unwrap_or_default()) {
                host_failures.observe_alive(result.as_ref().is_err_and(|e| failure_weight(e) > 0));
            }
            // 每次失败的尝试都马上记到主机头上，不等重试用完：以前一页要重试满两次才记一次，
            // 等重试的空档里同一主机的下一页照样派出去，死掉的主机要挂掉好几页才停（而且挂几页
            // 看重试等待和超时谁先到）。主机停掉时，它等着重试的页面也不再重试。种子页照旧在
            // 最后一次失败时记（它重试得多，失败了整个爬取也就结束了）。
            if !seed && let Err(e) = &result {
                let host = url.host_str().unwrap_or_default();
                let weight = failure_weight(e);
                if weight > 0 && !host_ok.contains(host) {
                    host_failures.add(host, weight);
                    if host_failures.blocked(&url) {
                        for (u, last) in retries.drop_host(host) {
                            let attachment = traces
                                .remove(u.as_str())
                                .is_some_and(|t| t.attachment_of.is_some());
                            if !attachment && refunds < req.limit {
                                refunds += 1;
                                scheduled -= 1;
                            }
                            let err = last.unwrap_or_else(|| {
                                ApiError::new(
                                    pagesift_api::ErrorCode::UpstreamError,
                                    "host dropped after repeated failures",
                                )
                            });
                            sink.error(u.as_str(), err).await;
                        }
                    }
                }
            }
            if stop.is_none()
                && (seed || !host_failures.blocked(&url))
                && retries.offer(&result, &url, depth, attempt, seed)
            {
                continue;
            }
            let trace = traces.remove(url.as_str());
            let is_attachment = trace.as_ref().is_some_and(|t| t.attachment_of.is_some());
            match result {
                // 错误状态页（404 模板、400 桩页）不是内容：不输出、不跟它的链接，只占用预算
                Ok(doc) if doc.metadata.status_code >= 400 => {
                    let status = doc.metadata.status_code;
                    sink.release_page(url.as_str()).await;
                    if !is_attachment && refunds < req.limit {
                        refunds += 1;
                        scheduled -= 1;
                    }
                    sink.error(
                        url.as_str(),
                        ApiError::new(
                            pagesift_api::ErrorCode::UpstreamError,
                            format!("HTTP {status}: not a content page"),
                        ),
                    )
                    .await;
                }
                Ok(mut doc) => {
                    // 和已经交付的某页全文相同（同一页的不同写法）：不交付、不占预算，
                    // 它的链接和那一页一样
                    if note_duplicate(&mut learner, &url, &mut doc) && !is_attachment {
                        if let Some(dup_of) = doc
                            .metadata
                            .pagesift
                            .as_ref()
                            .and_then(|p| p.duplicate_of.clone())
                        {
                            crate::graph::enrich_duplicate_provenance(
                                self.store_ref(),
                                graph_versions,
                                opts.owner.as_deref(),
                                &mut doc,
                                &dup_of,
                                "content_duplicate",
                            );
                        }
                        if let Some(gc) = graph_collector.as_mut() {
                            gc.ingest_provenance_only(&doc);
                        }
                        sink.release_page(url.as_str()).await;
                        if refunds < req.limit {
                            refunds += 1;
                            scheduled -= 1;
                        }
                        continue;
                    }
                    let base = doc
                        .metadata
                        .url
                        .as_deref()
                        .and_then(|u| Url::parse(u).ok())
                        .unwrap_or(url.clone());
                    host_ok.insert(url.host_str().unwrap_or_default().to_string());
                    // 跳转到的地址也算见过；两个地址跳到同一页时第二个不交付、不占预算
                    let landed = seen_hash(&base, ignore_query);
                    seen.insert(landed);
                    if !delivered.insert(landed) {
                        if let Some(gc) = graph_collector.as_mut() {
                            gc.ingest_provenance_only(&doc);
                        }
                        if !is_attachment {
                            scheduled -= 1;
                        }
                        continue;
                    }
                    let md = doc.markdown.as_deref().unwrap_or_default();
                    // 正文里的链接（导航、页脚不算）：枢纽页的判断按它，真实站点的大菜单有几百条链接
                    let contexts = link_contexts(md);
                    let lang = doc.metadata.language.as_deref();
                    if depth == 0 && url == start {
                        focus.note_seed_language(lang);
                    }
                    let page = focus.page_signal(
                        &base,
                        doc.metadata.title.as_deref(),
                        md,
                        contexts.len(),
                        lang,
                    );
                    // 这一页也计入本站统计量，再算它的相关度
                    focus.observe_page(&page);
                    // 权重变了：排队的链接按新权重重排（入队时各页的权重不一样，分数不可比）
                    if focus.weights_moved() {
                        queue.rescore(|s| focus.score(s));
                    }
                    let relevance = focus.relevance(&page);
                    if let Some(sig) = &learned_from
                        && focus.learn(sig, relevance >= MIN_RELEVANCE)
                    {
                        queue.rescore(|s| focus.score(s));
                    }
                    if stop.is_none()
                        && let Some(a) = adaptive.as_mut()
                    {
                        let before = a.stale();
                        let score = trace.as_ref().and_then(|t| t.score).unwrap_or(0.0);
                        let blind = trace.as_ref().is_some_and(|t| t.score.is_some())
                            && crate::focus::is_blind(score);
                        stop = a.observe(&focus.query, md, relevance, blind);
                        if !is_attachment {
                            let hit = a.last_fresh();
                            by_cluster.observe(cluster_key(&base), hit);
                            by_subtree.observe(subtree(&seed_host, &base), hit);
                        }
                        // 记下这一轮"没有新信息"的页面里分数最高的；有收获就清零
                        stale_max = if a.stale() > before {
                            stale_max.max(score)
                        } else {
                            0.0
                        };
                    }
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
                            let room = room_left(req.limit, scheduled, refunds);
                            for u in deferred.drain(..) {
                                if !scope.allows(&u) || seen.contains(&seen_hash(&u, ignore_query))
                                {
                                    continue;
                                }
                                let s = focus.link_signal(&u, None, None, PageSignal::default(), 0);
                                let scored = (focus.score(&s), s);
                                overflow |=
                                    offer(&mut queue, &mut seen, &mut fresh, u, 0, scored, room);
                            }
                        }
                    }
                    if stop.is_none() && follow_links && depth < max_depth && !is_attachment {
                        // 本站英文版的首页（`/en/`）是第二个首页：它的链接不多衰减一层；
                        // 记下已经找到，之后别的 `/xx/en/` 不再当英文入口
                        let portal = focus.is_language_portal(&base);
                        if portal {
                            focus.found_english_home();
                            // 别的语言的页面上排着的栏目链接不再拿保底分（见 Focus::english_target）
                            queue.rescore(|s| focus.score(s));
                        }
                        let link_depth = if portal { depth } else { depth + 1 };
                        // 导航、页脚里的链接不在正文 Markdown 里，锚文本从 DOM 的链接列表取
                        let anchors: HashMap<&str, &str> = doc
                            .links
                            .iter()
                            .flatten()
                            .zip(doc.link_text.iter().flatten())
                            .map(|(u, t)| (u.as_str(), t.as_str()))
                            .collect();
                        // 正文里指向同一个簇的链接数：几十条新闻、展品组成的列表整体降权。
                        // 只数正文里的：导航的大菜单往招生栏目链几十次，那是目录，不是列表
                        let mut per_cluster: HashMap<String, usize> = HashMap::new();
                        for link in contexts.keys() {
                            if let Some(u) = candidate(&base, link, &learner) {
                                *per_cluster.entry(cluster_key(&u)).or_default() += 1;
                            }
                        }
                        for link in doc.links.iter().flatten() {
                            let Some(u) = candidate(&base, link, &learner) else {
                                continue;
                            };
                            let h = seen_hash(&u, ignore_query);
                            // 开了附件收割：相关页面上同站点的文档走附件队列，不受路径范围限制
                            if let Some(att) = attachments.as_mut()
                                && is_document_url(&u)
                            {
                                let same_site = u
                                    .host_str()
                                    .is_some_and(|h| pagesift_content::site_key(h) == start_site);
                                if same_site && relevance >= MIN_RELEVANCE && !seen.contains(&h) {
                                    let ctx = contexts.get(link.split('#').next().unwrap_or(link));
                                    let anchor = ctx
                                        .map(|c| c.anchor.as_str())
                                        .or_else(|| anchors.get(link.as_str()).copied());
                                    let score =
                                        focus.link_score(&u, anchor, None, relevance, link_depth);
                                    parents.entry(h).or_insert_with(|| base.to_string());
                                    att.offer(h, u, depth + 1, score);
                                }
                                continue;
                            }
                            // 已经调度过的页面不再入队；还在排队的可以按更高的分数前移
                            if !scope.allows(&u) || (seen.contains(&h) && !queue.contains(h)) {
                                continue;
                            }
                            let ctx = contexts.get(link.split('#').next().unwrap_or(link));
                            let anchor = ctx
                                .map(|c| c.anchor.as_str())
                                .or_else(|| anchors.get(link.as_str()).copied())
                                .filter(|a| !a.is_empty());
                            let mut signal = focus.link_signal(
                                &u,
                                anchor,
                                ctx.map(|c| c.line.as_str()),
                                page,
                                link_depth,
                            );
                            if learner.demoted(&u) {
                                signal.scale(DEMOTED_SCORE);
                            }
                            let siblings = per_cluster.get(&cluster_key(&u)).copied().unwrap_or(1);
                            // 英文入口和院系子站首页同簇，但它不是列表里的一项
                            if siblings > crate::focus::LIST_FREE && !focus.is_language_portal(&u) {
                                signal.scale(
                                    (crate::focus::LIST_FREE as f32 / siblings as f32).sqrt(),
                                );
                            }
                            let scored = (focus.score(&signal), signal);
                            let room = room_left(req.limit, scheduled, refunds);
                            let was_queued = queue.contains(h);
                            overflow |= offer(
                                &mut queue,
                                &mut seen,
                                &mut fresh,
                                u,
                                depth + 1,
                                scored,
                                room,
                            );
                            // 第一次发现它的页面是父页；分数被抬高时父页不换
                            if queue.contains(h) && !was_queued {
                                parents.insert(h, base.to_string());
                            }
                        }
                        // 页面声明的英文版（hreflang）：语言切换常是脚本画的菜单，页面链接里没有它
                        for (lang, href) in &doc.alternates {
                            let Some(score) = focus.alternate_score(lang, depth) else {
                                continue;
                            };
                            let Some(u) = candidate(&base, href, &learner) else {
                                continue;
                            };
                            let h = seen_hash(&u, ignore_query);
                            if u == base || !scope.allows(&u) || seen.contains(&h) {
                                continue;
                            }
                            let room = room_left(req.limit, scheduled, refunds);
                            let scored = (score, LinkSignal::fixed(score));
                            overflow |=
                                offer(&mut queue, &mut seen, &mut fresh, u, depth, scored, room);
                            if queue.contains(h) {
                                parents.insert(h, base.to_string());
                            }
                        }
                    }
                    if !fresh.is_empty() {
                        sink.enqueued(std::mem::take(&mut fresh)).await;
                    }
                    // 队列头的链接比刚才那几页没用的页面分数都高（常见：栏目页刚抓回来，它的子页才入队）：
                    // 最好的候选还没试过，不算饱和。查询已经覆盖了的话，还要队首本身有查询的线索：
                    // 剩下的只是栏目保底分的链接时，再盲探下去没有意义
                    if let Some(a) = adaptive.as_mut()
                        && let Some(head) = queue.peek_score()
                        && head > stale_max
                        && (stop == Some("saturated")
                            || stop == Some("coverage_reached") && !crate::focus::is_blind(head))
                    {
                        a.forgive();
                        stale_max = 0.0;
                        stop = None;
                    }
                    // 查询已经覆盖、这一页没带来新东西，而能走的最好的链接也只是盲探（没有任何
                    // 查询的线索）：不用再数几页没收获，现在就停。还有带线索的候选时照常继续
                    if stop.is_none()
                        && let Some(a) = adaptive.as_ref()
                        && a.covered()
                        && !a.last_fresh()
                        && queue.peek_score().is_none_or(crate::focus::is_blind)
                    {
                        stop = Some("coverage_reached");
                    }
                    // 一直盲探、什么也没找到，但这一页刚带来了有线索的链接：先抓它们再说
                    if stop == Some("no_relevant_pages")
                        && queue
                            .peek_score()
                            .is_some_and(|s| !crate::focus::is_blind(s))
                    {
                        stop = None;
                    }
                    if let Some(w) = scope_note {
                        push_warning(&mut doc, w);
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
                    let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
                    ext.relevance = Some((relevance * 1000.0).round() / 1000.0);
                    ext.crawl = trace.map(|mut t| {
                        t.score = t.score.map(|s| (s * 1000.0).round() / 1000.0);
                        t
                    });
                    sink.settle_page(url.as_str()).await;
                    sink.page(doc).await;
                }
                Err(e) => {
                    // 别的页面每次失败时已经记过（见上面）
                    let weight = failure_weight(&e);
                    let host = url.host_str().unwrap_or_default();
                    if seed && weight > 0 && !host_ok.contains(host) {
                        host_failures.add(host, weight);
                    }
                    sink.release_page(url.as_str()).await;
                    if !is_attachment && refunds < req.limit {
                        refunds += 1;
                        scheduled -= 1;
                    }
                    sink.error(url.as_str(), e).await;
                }
            }
            let pending = if stop.is_some() { 0 } else { queue.len() };
            let att = attachments
                .as_ref()
                .map_or(0, |a| a.scheduled + a.pending());
            sink.total((scheduled + pending).min(req.limit) + att).await;
        }
        sink.release_pending().await;
        if let Some(gc) = graph_collector.take() {
            gc.finish(sink).await;
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
            pages: scheduled + attachments.as_ref().map_or(0, |a| a.scheduled),
        })
        .await;
        Ok(())
    }
}
