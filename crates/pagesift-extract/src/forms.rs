//! Read-only form discovery and explicit GET request planning. No submission,
//! invented credentials, JavaScript execution, or automatic write actions.
use std::collections::{BTreeMap, BTreeSet};

use pagesift_html::{Html, NodeRef};
use serde::Serialize;
use url::Url;

#[derive(Clone, Debug, Serialize)]
pub struct Form {
    pub index: usize,
    pub id: Option<String>,
    pub action: String,
    pub method: String,
    pub controls: Vec<Control>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Control {
    pub name: Option<String>,
    pub kind: String,
    pub label: String,
    pub required: bool,
    pub disabled: bool,
    pub read_only: bool,
    pub sensitive: bool,
    // Values (including hidden CSRF tokens) are never exported in metadata.
}

const MAX_HTML: usize = 4 * 1024 * 1024;
const MAX_CONTROLS: usize = 2048;

pub fn discover_forms(html: &str, base: &Url) -> Result<Vec<Form>, String> {
    let doc = parse(html)?;
    let forms = form_nodes(&doc)?;
    let controls = controls(&doc)?;
    let mut labels = BTreeMap::<&str, Vec<String>>::new();
    for node in doc.root().descendants() {
        if let Some(e) = node.as_element().filter(|e| e.name() == "label")
            && let Some(id) = e.attr("for")
        {
            let entry = labels.entry(id).or_default();
            if entry.len() < 4 {
                entry.push(label_text(node));
            }
        }
    }
    forms
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let el = node.as_element().unwrap();
            let action = action(*node, &doc, base)?;
            let mut fields = Vec::new();
            for n in &controls {
                if owner(*n, &forms) != Some(*node) {
                    continue;
                }
                let e = n.as_element().unwrap();
                let kind = kind(*n);
                let label = e.attr("aria-label").map(str::to_owned).unwrap_or_else(|| {
                    let id = e.attr("id");
                    let mut texts = id
                        .and_then(|id| labels.get(id))
                        .cloned()
                        .unwrap_or_default();
                    for l in n
                        .ancestors()
                        .filter(|a| {
                            a.as_element()
                                .is_some_and(|e| e.name() == "label" && e.attr("for").is_none())
                        })
                        .take(4)
                    {
                        texts.push(label_text(l));
                    }
                    texts.join(" ")
                });
                fields.push(Control {
                    name: e.attr("name").map(str::to_owned),
                    kind: kind.clone(),
                    label: label.chars().take(512).collect(),
                    required: e.attr("required").is_some(),
                    disabled: disabled(*n),
                    read_only: e.attr("readonly").is_some(),
                    sensitive: matches!(kind.as_str(), "password" | "hidden" | "file"),
                });
            }
            Ok(Form {
                index,
                id: el.attr("id").map(str::to_owned),
                action: action.to_string(),
                method: method(*node).into(),
                controls: fields,
            })
        })
        .collect()
}

/// Construct a GET URL only on an explicit call. Duplicate field names and
/// multi-select values retain document order. The caller must apply its normal
/// scope/robots/SSRF rules before fetching the returned URL.
pub fn plan_form_get(
    html: &str,
    base: &Url,
    index: usize,
    replacements: &BTreeMap<String, Vec<String>>,
) -> Result<Url, String> {
    let doc = parse(html)?;
    let forms = form_nodes(&doc)?;
    let form = *forms.get(index).ok_or("unknown form index")?;
    if method(form) != "get" {
        return Err("only explicit GET form plans are supported".into());
    }
    let mut url = action(form, &doc, base)?;
    let controls = controls(&doc)?;
    let mut known = BTreeSet::new();
    let mut replaced = BTreeSet::new();
    let mut pairs = Vec::new();
    for n in controls {
        if owner(n, &forms) != Some(form) || disabled(n) {
            continue;
        }
        let e = n.as_element().unwrap();
        let Some(name) = e.attr("name").filter(|s| !s.is_empty()) else {
            continue;
        };
        let k = kind(n);
        if matches!(k.as_str(), "submit" | "button" | "reset" | "image") {
            continue;
        }
        known.insert(name.to_owned());
        if matches!(k.as_str(), "password" | "file") {
            return Err("GET plans reject password and file controls".into());
        }
        if let Some(values) = replacements.get(name) {
            if e.attr("readonly").is_some() {
                return Err("cannot replace a read-only field".into());
            }
            if replaced.insert(name.to_owned()) {
                if e.attr("required").is_some()
                    && (values.is_empty() || values.iter().any(String::is_empty))
                {
                    return Err("required field is empty".into());
                }
                pairs.extend(values.iter().cloned().map(|v| (name.to_owned(), v)));
            }
            continue;
        }
        if matches!(k.as_str(), "radio" | "checkbox") && e.attr("checked").is_none() {
            continue;
        }
        let values = if e.name() == "select" {
            let options: Vec<_> = n
                .descendants()
                .filter(|o| {
                    o.as_element().is_some_and(|e| {
                        e.name() == "option"
                            && e.attr("disabled").is_none()
                            && !o.ancestors().any(|a| {
                                a.as_element().is_some_and(|e| {
                                    e.name() == "optgroup" && e.attr("disabled").is_some()
                                })
                            })
                    })
                })
                .collect();
            let selected: Vec<_> = options
                .iter()
                .copied()
                .filter(|o| o.as_element().unwrap().attr("selected").is_some())
                .collect();
            let selected = if e.attr("multiple").is_some() {
                selected
            } else {
                selected
                    .last()
                    .copied()
                    .or_else(|| options.first().copied())
                    .into_iter()
                    .collect()
            };
            selected
                .iter()
                .map(|o| {
                    o.as_element()
                        .unwrap()
                        .attr("value")
                        .map(str::to_owned)
                        .unwrap_or_else(|| o.text())
                })
                .collect()
        } else if e.name() == "textarea" {
            vec![n.text()]
        } else {
            vec![
                e.attr("value")
                    .unwrap_or(if matches!(k.as_str(), "checkbox" | "radio") {
                        "on"
                    } else {
                        ""
                    })
                    .into(),
            ]
        };
        if e.attr("required").is_some()
            && (values.is_empty() || values.iter().any(String::is_empty))
        {
            return Err("required field is empty".into());
        }
        pairs.extend(values.into_iter().map(|v| (name.to_owned(), v)));
    }
    if replacements.keys().any(|k| !known.contains(k)) {
        return Err("unknown or disabled replacement field".into());
    }
    // HTML GET submissions replace the action query, rather than append it.
    url.set_query(None);
    url.set_fragment(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    if url.as_str().len() > 16 * 1024 {
        return Err("form query exceeds 16 KiB".into());
    }
    Ok(url)
}

fn parse(html: &str) -> Result<Html<'_>, String> {
    if html.len() > MAX_HTML {
        return Err("form document exceeds 4 MiB".into());
    }
    let doc = Html::parse(html);
    for (i, n) in doc.root().descendants().enumerate() {
        if i >= 50_000 || n.ancestors().take(129).count() > 128 {
            return Err("form DOM node/depth budget exceeded".into());
        }
    }
    Ok(doc)
}
fn label_text(node: NodeRef<'_>) -> String {
    node.descendants()
        .take(256)
        .filter_map(|n| n.as_text())
        .flat_map(|t| t.chars())
        .take(512)
        .collect()
}
fn form_nodes<'a>(doc: &'a Html<'a>) -> Result<Vec<NodeRef<'a>>, String> {
    let nodes: Vec<_> = doc
        .root()
        .descendants()
        .filter(|n| n.as_element().is_some_and(|e| e.name() == "form"))
        .take(129)
        .collect();
    if nodes.len() > 128 {
        return Err("too many forms".into());
    }
    Ok(nodes)
}
fn controls<'a>(doc: &'a Html<'a>) -> Result<Vec<NodeRef<'a>>, String> {
    let nodes: Vec<_> = doc
        .root()
        .descendants()
        .filter(|n| {
            n.as_element()
                .is_some_and(|e| matches!(e.name(), "input" | "textarea" | "select" | "button"))
        })
        .take(MAX_CONTROLS + 1)
        .collect();
    if nodes.len() > MAX_CONTROLS {
        return Err("too many form controls".into());
    }
    Ok(nodes)
}
fn owner<'a>(n: NodeRef<'a>, forms: &[NodeRef<'a>]) -> Option<NodeRef<'a>> {
    if let Some(id) = n.as_element()?.attr("form") {
        let mut matching = forms
            .iter()
            .copied()
            .filter(|f| f.as_element().unwrap().attr("id") == Some(id));
        let found = matching.next();
        return if matching.next().is_none() {
            found
        } else {
            None
        };
    }
    n.ancestors()
        .find(|a| a.as_element().is_some_and(|e| e.name() == "form"))
}
fn disabled(n: NodeRef<'_>) -> bool {
    n.as_element().unwrap().attr("disabled").is_some()
        || n.ancestors().any(|a| {
            a.as_element()
                .is_some_and(|e| e.name() == "fieldset" && e.attr("disabled").is_some())
                && !a
                    .children()
                    .find(|c| c.as_element().is_some_and(|e| e.name() == "legend"))
                    .is_some_and(|l| n.ancestors().any(|ancestor| ancestor == l))
        })
}
fn kind(n: NodeRef<'_>) -> String {
    let e = n.as_element().unwrap();
    if e.name() == "input" {
        e.attr("type").unwrap_or("text").to_ascii_lowercase()
    } else if e.name() == "button" {
        e.attr("type").unwrap_or("submit").to_ascii_lowercase()
    } else {
        e.name().into()
    }
}
fn method(n: NodeRef<'_>) -> &'static str {
    match n
        .as_element()
        .unwrap()
        .attr("method")
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "post" => "post",
        "dialog" => "dialog",
        _ => "get",
    }
}
fn action(n: NodeRef<'_>, doc: &Html<'_>, base: &Url) -> Result<Url, String> {
    let effective_base = doc
        .root()
        .descendants()
        .find_map(|n| {
            n.as_element()
                .filter(|e| e.name() == "base")
                .and_then(|e| e.attr("href"))
        })
        .and_then(|s| base.join(s).ok())
        .unwrap_or_else(|| base.clone());
    let target = n.as_element().unwrap().attr("action").unwrap_or("");
    let url = if target.trim().is_empty() {
        base.clone()
    } else {
        effective_base
            .join(target)
            .map_err(|_| "invalid form action")?
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("form action must be credential-free HTTP(S)".into());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_owners_disabled_fieldsets_and_repeated_values() {
        let html = r#"<base href='/base/'><form id='s' action='find?old=1'>
            <fieldset disabled><legend><input name='legend' value='ok'></legend><input name='blocked' value='no'></fieldset>
            <label for='q'>Search</label><input id='q' name='q' required>
            <input type='checkbox' name='tag' value='a' checked><input type='checkbox' name='tag' value='b'>
            <select multiple name='color'><option selected value='red'>R</option><optgroup disabled><option selected value='bad'>B</option></optgroup><option selected>green</option></select>
            <input type='hidden' name='csrf' value='SECRET'></form><input form='s' name='outside' value='yes'>"#;
        let base = Url::parse("https://example.test/page").unwrap();
        let f = discover_forms(html, &base).unwrap();
        assert_eq!(
            f[0].controls
                .iter()
                .find(|c| c.name.as_deref() == Some("q"))
                .unwrap()
                .label,
            "Search"
        );
        assert!(!serde_json::to_string(&f).unwrap().contains("SECRET"));
        let replacements = BTreeMap::from([("q".into(), vec!["東京 & rust".into()])]);
        let url = plan_form_get(html, &base, 0, &replacements).unwrap();
        assert_eq!(url.path(), "/base/find");
        assert_eq!(
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect::<Vec<_>>(),
            [
                ("legend", "ok"),
                ("q", "東京 & rust"),
                ("tag", "a"),
                ("color", "red"),
                ("color", "green"),
                ("csrf", "SECRET"),
                ("outside", "yes")
            ]
            .map(|(k, v)| (k.into(), v.into()))
        );
    }
    #[test]
    fn no_write_or_credential_submission_or_silent_unknown_fields() {
        let base = Url::parse("https://example.test/").unwrap();
        for html in [
            "<form method='POST'><input name='q'></form>",
            "<form><input name='p' type='password'></form>",
            "<form action='javascript:alert(1)'></form>",
        ] {
            assert!(plan_form_get(html, &base, 0, &BTreeMap::new()).is_err());
        }
        assert!(
            plan_form_get(
                "<form><input name='q' required></form>",
                &base,
                0,
                &BTreeMap::new()
            )
            .is_err()
        );
        assert!(
            plan_form_get(
                "<form></form>",
                &base,
                0,
                &BTreeMap::from([("unknown".into(), vec!["x".into()])])
            )
            .is_err()
        );
    }
}
