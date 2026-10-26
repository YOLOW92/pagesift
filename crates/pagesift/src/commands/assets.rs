//! 本地文件、媒体、流和会话下载类子命令。

use crate::*;

pub(crate) fn run(command: Command, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match command {
        Command::Inspect { file, base_url } => {
            let html = read_bounded(file, 4 * 1024 * 1024)?;
            let forms =
                pagesift_extract::discover_forms(&html, &base_url).map_err(anyhow::Error::msg)?;
            let knowledge = pagesift_extract::KnowledgeExtractor::builtin()
                .extract(&html, &[])
                .map_err(anyhow::Error::msg)?;
            println!(
                "{}",
                serde_json::json!({"forms":forms,"knowledge":knowledge})
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Relocate {
            file,
            original,
            selector,
        } => {
            use pagesift_extract::{FuzzyAnchor, FuzzyMatch, FuzzyOptions};
            let old = read_bounded(Some(original), 4 * 1024 * 1024)?;
            let new = read_bounded(Some(file), 4 * 1024 * 1024)?;
            let old = pagesift_html::Html::parse(&old);
            let new = pagesift_html::Html::parse(&new);
            let anchor = FuzzyAnchor::capture(&old, &selector).map_err(anyhow::Error::msg)?;
            let result = match anchor
                .locate(&new, FuzzyOptions::default())
                .map_err(anyhow::Error::msg)?
            {
                FuzzyMatch::Candidate {
                    node,
                    score,
                    runner_up,
                } => {
                    serde_json::json!({"status":"candidate","score":score,"runner_up":runner_up,"text":node.text(),"html":node.html()})
                }
                FuzzyMatch::LowConfidence { best_score } => {
                    serde_json::json!({"status":"low_confidence","score":best_score})
                }
                FuzzyMatch::Ambiguous {
                    best_score,
                    runner_up,
                } => {
                    serde_json::json!({"status":"ambiguous","score":best_score,"runner_up":runner_up})
                }
                FuzzyMatch::BudgetExceeded => serde_json::json!({"status":"budget_exceeded"}),
            };
            println!("{result}");
            Ok(ExitCode::SUCCESS)
        }
        Command::DiscoverJs {
            url,
            max_files,
            max_depth,
        } => {
            let policy = configured_policy(&cfg)?;
            let graph = runtime()?.block_on(async {
                let (final_url, html) = fetch_text(&policy, &url, 4 * 1024 * 1024).await?;
                pagesift::scripts::discover_script_graph(
                    &policy,
                    &html,
                    &final_url,
                    pagesift::scripts::ScriptBudget {
                        max_files,
                        max_depth,
                        ..Default::default()
                    },
                )
                .await
            })?;
            println!("{}", serde_json::to_string(&graph)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::MediaExtract {
            file,
            base_url,
            adapter,
        } => {
            let body = read_bounded(file, 4 * 1024 * 1024)?;
            let items = pagesift::media::extract_media(adapter, &base_url, &body)?;
            println!("{}", serde_json::to_string(&items)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::MediaArchive {
            url,
            adapter,
            out,
            template,
            max_bytes,
        } => {
            anyhow::ensure!(
                (1..=2 * 1024 * 1024 * 1024).contains(&max_bytes),
                "media byte budget must be 1..=2 GiB"
            );
            let policy = configured_policy(&cfg)?;
            let results = runtime()?.block_on(async {
                let (url, body) = fetch_text(&policy, &url, 4 * 1024 * 1024).await?;
                let items = pagesift::media::extract_media(adapter, &url, &body)?;
                for item in &items {
                    pagesift::postprocess::media_filename(&template, item, &"0".repeat(64))?;
                }
                let mut archive = pagesift::download::Archive::open(out.join("archive"))?;
                let mut total = 0;
                let mut results = Vec::new();
                for item in items {
                    anyhow::ensure!(total < max_bytes, "media byte budget exhausted");
                    let request = pagesift::download::DownloadRequest {
                        source: item.source.clone(),
                        id: item.id.clone(),
                        revision: item.revision.clone(),
                        url: Url::parse(&item.url)?,
                        max_bytes: max_bytes - total,
                        expected_blake3: None,
                        restart: false,
                    };
                    let downloaded = archive.download(&policy, &request).await?;
                    total += downloaded.bytes;
                    results.push(pagesift::postprocess::publish_media(
                        &item,
                        &downloaded,
                        &out.join("files"),
                        &template,
                    )?);
                }
                Ok::<_, anyhow::Error>(results)
            })?;
            println!("{}", serde_json::to_string(&results)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::StreamPlan { file, base_url } => {
            let body = read_bounded(file, 1024 * 1024)?;
            println!(
                "{}",
                serde_json::to_string(&pagesift::stream::plan_stream(&body, &base_url)?)?
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::StreamDownload {
            url,
            out,
            variant,
            max_bytes,
        } => {
            let policy = configured_policy(&cfg)?;
            let results = runtime()?.block_on(async {
                let (mut url, body) = fetch_text(&policy, &url, 1024 * 1024).await?;
                let mut plan = pagesift::stream::plan_stream(&body, &url)?;
                if let pagesift::stream::StreamPlan::Master { variants } = &plan {
                    let index = variant.context("HLS master requires explicit --variant")?;
                    let next =
                        Url::parse(&variants.get(index).context("unknown variant index")?.url)?;
                    let (next, body) = fetch_text(&policy, &next, 1024 * 1024).await?;
                    url = next;
                    plan = pagesift::stream::plan_stream(&body, &url)?;
                } else {
                    anyhow::ensure!(variant.is_none(), "--variant requires a master playlist");
                }
                pagesift::stream::archive_stream(&policy, &plan, &url, &out, max_bytes).await
            })?;
            println!("{}", serde_json::to_string(&results)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::SessionFetch {
            url,
            state,
            reset_session,
            format,
            full,
            allow,
        } => {
            let mut sc = cfg.scraper(false)?;
            allow.apply(&mut sc);
            let fetcher = Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?);
            let policy = pagesift_policy::Policy::new(fetcher, sc.policy);
            let state = pagesift::session::SessionFile::open(
                state,
                &url,
                Default::default(),
                reset_session,
            )?;
            let session = state.session();
            let response = runtime()?.block_on(policy.fetch(
                pagesift_net::FetchRequest::new(url.as_str()).with_session(session.clone()),
                pagesift_policy::RobotsMode::Respect,
            ));
            // Redirect cookies and error/retirement accounting also survive a failed request.
            state.save()?;
            let response = response?;
            anyhow::ensure!(
                (200..300).contains(&response.status),
                "HTTP status {}",
                response.status
            );
            anyhow::ensure!(!response.truncated, "session response exceeds body limit");
            let document = process(
                &response.text().text,
                &Options {
                    base_url: Some(response.final_url),
                    only_main_content: !full,
                    ..Default::default()
                },
            );
            if let Some(barrier) = &document.barrier {
                session.retire();
                state.save()?;
                anyhow::bail!("access barrier: {}", barrier.kind.as_str());
            }
            if format == "json" {
                println!(
                    "{}",
                    serde_json::json!({"markdown":document.markdown,"session":session.stats()})
                );
            } else {
                print!("{}", document.markdown);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Download {
            url,
            source,
            id,
            revision,
            out,
            max_bytes,
            expected_blake3,
            restart,
            allow,
        } => {
            let mut sc = cfg.scraper(false)?;
            allow.apply(&mut sc);
            let fetcher = Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?);
            let policy = pagesift_policy::Policy::new(fetcher, sc.policy);
            let mut archive = pagesift::download::Archive::open(out)?;
            let request = pagesift::download::DownloadRequest {
                url,
                source,
                id,
                revision,
                max_bytes,
                expected_blake3,
                restart,
            };
            let result = runtime()?.block_on(archive.download(&policy, &request))?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(ExitCode::SUCCESS)
        }
        _ => unreachable!(),
    }
}
