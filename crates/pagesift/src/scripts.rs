//! Opt-in, origin-scoped external module traversal. Candidates carry provenance
//! and remain separate from observed browser traffic or confirmed GET requests.
use std::collections::{BTreeSet, VecDeque};

use anyhow::{Result, ensure};
use pagesift_html::Html;
use pagesift_net::FetchRequest;
use pagesift_policy::{Policy, RobotsMode};
use serde::Serialize;
use url::Url;

#[derive(Clone, Copy, Debug)]
pub struct ScriptBudget {
    pub max_files: usize,
    pub max_depth: usize,
    pub max_bytes: usize,
}
impl Default for ScriptBudget {
    fn default() -> Self {
        Self {
            max_files: 32,
            max_depth: 3,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ScriptEdge {
    pub source: String,
    pub url: String,
    pub kind: &'static str,
}
#[derive(Debug, Default, Serialize)]
pub struct ScriptGraph {
    pub attempted_files: usize,
    pub fetched_files: usize,
    pub fetched_bytes: usize,
    pub edges: Vec<ScriptEdge>,
    pub failures: Vec<String>,
    pub incomplete: bool,
}

/// The root HTML is supplied by the caller, with its final URL. Same-origin
/// checks also apply before redirect sends; all fetches use the normal policy.
pub async fn discover_script_graph(
    policy: &Policy,
    html: &str,
    page: &Url,
    budget: ScriptBudget,
) -> Result<ScriptGraph> {
    ensure!(html.len() <= 4 * 1024 * 1024, "HTML exceeds 4 MiB");
    ensure!(
        (1..=128).contains(&budget.max_files)
            && budget.max_depth <= 8
            && (1..=16 * 1024 * 1024).contains(&budget.max_bytes),
        "invalid script budget"
    );
    let doc = Html::parse(html);
    let base = doc
        .root()
        .descendants()
        .find_map(|n| {
            n.as_element()
                .filter(|e| e.name() == "base")
                .and_then(|e| e.attr("href"))
        })
        .and_then(|s| page.join(s).ok())
        .unwrap_or_else(|| page.clone());
    let mut queue = VecDeque::new();
    let mut seen = BTreeSet::new();
    let mut out = ScriptGraph::default();
    let mut inline_bytes = 0;
    for node in doc.root().descendants() {
        let Some(e) = node.as_element() else { continue };
        if e.name() != "script"
            || e.attr("type").is_some_and(|s| {
                !matches!(
                    s.trim(),
                    "" | "module" | "text/javascript" | "application/javascript"
                )
            })
        {
            continue;
        }
        if let Some(src) = e.attr("src") {
            if let Some(u) = scoped(&base, src, page) {
                enqueue(&mut queue, &mut seen, u, 0, &mut out);
            }
        } else {
            let source = node.text();
            inline_bytes += source.len();
            if inline_bytes > 2 * 1024 * 1024 {
                out.incomplete = true;
                break;
            }
            let scan = pagesift_content::scan_script(&source, &base);
            out.incomplete |= scan.budget_exceeded;
            for url in scan.navigation {
                if scoped(page, &url, page).is_some() {
                    edge(&mut out, page, url, "static-navigation");
                }
            }
            for u in scan.dependencies {
                if let Some(u) = scoped(page, &u, page) {
                    enqueue(&mut queue, &mut seen, u, 0, &mut out);
                }
            }
        }
    }
    while let Some((url, depth)) = queue.pop_front() {
        if out.attempted_files >= budget.max_files || out.fetched_bytes >= budget.max_bytes {
            out.incomplete = true;
            break;
        }
        let mut req = FetchRequest::new(url.as_str());
        req.same_origin_redirects = true;
        req.max_body_bytes = Some((budget.max_bytes - out.fetched_bytes).min(1024 * 1024));
        out.attempted_files += 1;
        let response = match policy.fetch(req, RobotsMode::Respect).await {
            Ok(r) => r,
            Err(e) => {
                out.failures
                    .push(format!("{}: {e}", pagesift_net::redact_url(url.as_str())));
                out.incomplete = true;
                continue;
            }
        };
        out.fetched_files += 1;
        out.fetched_bytes += response.body.len();
        if response.truncated
            || response.status != 200
            || response.final_url.origin() != page.origin()
        {
            out.failures.push(format!(
                "{}: incomplete or unsuccessful script",
                pagesift_net::redact_url(url.as_str())
            ));
            out.incomplete = true;
            continue;
        }
        let mime = response
            .content_type()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !matches!(
            mime,
            "text/javascript"
                | "application/javascript"
                | "application/ecmascript"
                | "text/ecmascript"
        ) {
            out.failures.push(format!(
                "{}: not JavaScript MIME",
                pagesift_net::redact_url(url.as_str())
            ));
            out.incomplete = true;
            continue;
        }
        let source = response.text().text;
        let navigation = pagesift_content::scan_script(&source, &base);
        let modules = pagesift_content::scan_script(&source, &response.final_url);
        out.incomplete |= navigation.budget_exceeded || modules.budget_exceeded;
        for u in navigation.navigation {
            if scoped(page, &u, page).is_some() {
                edge(&mut out, &response.final_url, u, "static-navigation");
            }
        }
        for u in modules.dependencies {
            if let Some(u) = scoped(page, &u, page) {
                edge(&mut out, &response.final_url, u.to_string(), "module");
                if depth < budget.max_depth {
                    enqueue(&mut queue, &mut seen, u, depth + 1, &mut out);
                } else if !seen.contains(u.as_str()) {
                    out.incomplete = true;
                }
            }
        }
    }
    Ok(out)
}
fn scoped(base: &Url, value: &str, page: &Url) -> Option<Url> {
    let mut u = base.join(value).ok()?;
    if !matches!(u.scheme(), "http" | "https")
        || u.origin() != page.origin()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return None;
    }
    u.set_fragment(None);
    Some(u)
}
fn enqueue(
    queue: &mut VecDeque<(Url, usize)>,
    seen: &mut BTreeSet<String>,
    url: Url,
    depth: usize,
    out: &mut ScriptGraph,
) {
    if seen.len() >= 1024 {
        out.incomplete = true;
        return;
    }
    if seen.insert(url.to_string()) {
        queue.push_back((url, depth));
    }
}
fn edge(out: &mut ScriptGraph, source: &Url, url: String, kind: &'static str) {
    if out.edges.len() >= 4096 {
        out.incomplete = true;
        return;
    }
    if !out
        .edges
        .iter()
        .any(|e| e.source == source.as_str() && e.url == url && e.kind == kind)
    {
        out.edges.push(ScriptEdge {
            source: source.to_string(),
            url,
            kind,
        });
    }
}
