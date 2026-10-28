//! Conservative, local evidence for otherwise unmarked editorial regions.
use crate::{dom::NodeR, extract::Skipper};
use pagesift_html::{Html, Tag};

fn tokens(node: &NodeR<'_>, wanted: &[&str]) -> bool {
    node.as_element().is_some_and(|e| {
        [e.attr("class"), e.attr("id"), e.attr("itemtype")]
            .into_iter()
            .flatten()
            .any(|s| {
                s.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|t| wanted.iter().any(|w| t.eq_ignore_ascii_case(w)))
            })
    })
}

/// Exceptions are scoped to a compact attachment inside an article. Negative
/// ancestors still win, and the renderer still applies hard visibility rules.
pub(crate) fn attachment(node: &NodeR<'_>) -> bool {
    let up = || std::iter::once(*node).chain(node.ancestors()).take(10);
    // 每个元素都要查，绝大多数不在 <article> 里：先不分配地看一眼
    if !up().any(|n| n.tag() == Some(Tag::Article)) {
        return false;
    }
    let chain: Vec<_> = up().collect();
    if chain.iter().any(|n| {
            tokens(
                n,
                &[
                    "related",
                    "recommended",
                    "sponsored",
                    "advertisement",
                    "playlist",
                    "control",
                    "toolbar",
                ],
            )
        })
    {
        return false;
    }
    if quotation(node) || procedural_product(node) {
        return true;
    }
    chain
        .iter()
        .take_while(|n| n.tag() != Some(Tag::Article))
        .any(|root| {
            let product = tokens(root, &["product"]) && tokens(root, &["callout"]);
            let video = root.tag() == Some(Tag::Figure) || tokens(root, &["video"]);
            if !product && !video {
                return false;
            }
            let nodes: Vec<_> = root.descendants().take(129).collect();
            if nodes.len() > 128 {
                return false;
            }
            let chars = root.text().chars().count();
            if !(20..=2400).contains(&chars) {
                return false;
            }
            if product {
                return nodes.iter().any(|n| tokens(n, &["description", "price"]))
                    && nodes.iter().any(|n| {
                        n.as_element()
                            .is_some_and(|e| e.tag() == Tag::A && e.attr("href").is_some())
                    });
            }
            let media = nodes.iter().any(|n| {
                n.tag() == Some(Tag::Video)
                    || n.as_element().is_some_and(|e| {
                        e.attr("data-video-id").is_some() || e.attr("data-vid").is_some()
                    })
            });
            let caption = nodes.iter().any(|n| {
                n.tag() == Some(Tag::Figcaption) || tokens(n, &["caption", "description"])
            });
            media && caption
        })
}

/// A title-matched short heading plus substantial *direct* prose can identify a
/// legacy layout without article metadata. Multiple independent candidates abstain.
pub(crate) fn unmarked_root<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<NodeR<'a>> {
    let title = dom
        .root()
        .descendants()
        .find(|n| n.tag() == Some(Tag::Title))?
        .text();
    let signature = crate::article::TitleSignature::new(&title);
    let mut found = None;
    for heading in dom
        .body()
        .descendants()
        .filter(|n| matches!(n.tag(), Some(Tag::P | Tag::H1 | Tag::H2 | Tag::Div)))
    {
        let (chars, linked, _) = skipper.stats(&heading);
        if !(12..=180).contains(&chars)
            || linked != 0
            || !crate::article::title_agrees_with(&signature, &heading.text())
            || heading.ancestors().any(|a| skipper.skip(&a))
        {
            continue;
        }
        for parent in heading.ancestors().take(3) {
            if !matches!(parent.tag(), Some(Tag::Div | Tag::Section | Tag::Main)) {
                break;
            }
            let (size, links, count) = skipper.stats(&parent);
            if !(250..=3500).contains(&size) || links * 5 > size || count > 16 {
                continue;
            }
            let direct: String = parent.children().filter_map(|n| n.as_text()).collect();
            let sentences = direct
                .chars()
                .filter(|c| matches!(c, '.' | '。' | '!' | '！' | '?' | '？'))
                .count();
            if crate::dom::count_non_whitespace(&direct) < 200 || sentences < 3 {
                continue;
            }
            if found.is_some_and(|old| old != parent) {
                return None;
            }
            found = Some(parent);
            break;
        }
    }
    found
}

/// Compact embedded quotations retain their own text even when the publisher
/// labels the wrapper as an embed. Never protect a larger recommendation tree.
pub(crate) fn quotation(node: &NodeR<'_>) -> bool {
    let shaped = node.tag() == Some(Tag::Blockquote)
        || tokens(node, &["blockquote"])
            && node
                .ancestors()
                .take(3)
                .any(|n| n.tag() == Some(Tag::Blockquote))
        || matches!(node.tag(), Some(Tag::Div | Tag::Section))
            && node.children().any(|n| n.tag() == Some(Tag::Blockquote))
            && node.children().all(|n| {
                n.is_comment()
                    || n.as_text().is_some_and(|t| t.trim().is_empty())
                    || matches!(n.tag(), Some(Tag::Blockquote | Tag::Script))
            });
    if !shaped {
        return false;
    }
    let nodes: Vec<_> = node.descendants().take(129).collect();
    if nodes.len() > 128 {
        return false;
    }
    let text = crate::dom::count_non_whitespace(&node.text());
    let linked: usize = nodes
        .iter()
        .filter(|n| n.tag() == Some(Tag::A))
        .map(|n| crate::dom::count_non_whitespace(&n.text()))
        .sum();
    text.saturating_sub(linked) >= 120 && linked * 2 <= text
}

/// Explicit recommendation attribution in link query metadata is stronger than
/// an incidental word in prose. Require several links and a majority of them.
pub(crate) fn recommendation_list(node: &NodeR<'_>) -> bool {
    if !matches!(node.tag(), Some(Tag::Ul | Tag::Ol)) {
        return false;
    }
    let mut links = 0;
    let mut related = 0;
    for n in node.descendants().take(256) {
        if n.tag() != Some(Tag::A) {
            continue;
        }
        let Some(href) = n.as_element().and_then(|e| e.attr("href")) else {
            continue;
        };
        links += 1;
        if href.split_once('?').is_some_and(|(_, query)| {
            let q = query.to_ascii_lowercase();
            q.contains("related") || q.contains("recommend")
        }) {
            related += 1;
        }
    }
    links >= 3 && related * 2 >= links
}

/// A standalone purchase link can be a named item in an editorial procedure.
/// Require an immediately preceding explanatory paragraph and a nearby heading.
pub(crate) fn procedural_product(node: &NodeR<'_>) -> bool {
    if node.tag() != Some(Tag::P) || !node.ancestors().any(|n| n.tag() == Some(Tag::Article)) {
        return false;
    }
    let links: Vec<_> = node
        .descendants()
        .filter(|n| n.tag() == Some(Tag::A))
        .take(2)
        .collect();
    if links.len() != 1 {
        return false;
    }
    let Some(href) = links[0].as_element().and_then(|e| e.attr("href")) else {
        return false;
    };
    let path = href.split('?').next().unwrap_or_default();
    if !["/shop/", "/product/", "/products/", "/buy/"]
        .iter()
        .any(|p| path.contains(p))
    {
        return false;
    }
    let mut previous = node.prev_sibling();
    let mut elements = Vec::new();
    while let Some(n) = previous {
        previous = n.prev_sibling();
        if n.is_element() {
            elements.push(n);
        }
        if elements.len() == 2 {
            break;
        }
    }
    elements.len() == 2
        && elements[0].tag() == Some(Tag::P)
        && !elements[0].descendants().any(|n| n.tag() == Some(Tag::A))
        && elements[0].text().chars().count() >= 12
        && matches!(elements[1].tag(), Some(Tag::H2 | Tag::H3 | Tag::H4))
}
