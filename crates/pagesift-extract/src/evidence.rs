//! DataGraph evidence adapters bridging extractors in `pagesift-extract` to `pagesift_graph::evidence`.

use pagesift_graph::evidence::{ExtractionMethod, RawFieldTrace, collect_json_traces};
use pagesift_html::Html;
use serde::Serialize;
use serde_json::Value;

use crate::adaptive::{AdaptiveOutcome, FieldRelocation, SelectorMemory, resolve_adaptive_schema};
use crate::engine::EngineTrace;
use crate::patterns::{RegexExtractor, RegexOutput};
use crate::structured::{StructuredData, extract_structured};
use crate::{Error, JsonCssExtractor, Schema};

/// Result of adaptive CSS extraction bundled with selector-level evidence traces.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AdaptiveWithEvidence {
    pub outcome: AdaptiveOutcome,
    pub traces: Vec<RawFieldTrace>,
}

/// Result of structured metadata extraction bundled with entity-level evidence traces.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StructuredWithEvidence {
    pub data: StructuredData,
    pub traces: Vec<RawFieldTrace>,
}

/// Adaptive extraction preserving relocated selector provenance on each extracted field trace.
pub fn extract_adaptive_with_evidence(
    doc: &Html<'_>,
    schema: &Schema,
    memory: &dyn SelectorMemory,
    source_url: Option<&str>,
) -> Result<AdaptiveWithEvidence, Error> {
    let (mut outcome, patched, empty_base) = resolve_adaptive_schema(doc, schema, memory)?;
    if empty_base {
        return Ok(AdaptiveWithEvidence {
            outcome,
            traces: Vec::new(),
        });
    }
    let extractor = JsonCssExtractor::new(&patched)?;
    let (items, mut traces) = extractor.extract_document_with_evidence(doc, source_url);
    if !outcome.relocated.is_empty() {
        apply_relocations(&mut traces, &outcome.relocated);
    }
    outcome.items = items;
    Ok(AdaptiveWithEvidence { outcome, traces })
}

fn apply_relocations(traces: &mut [RawFieldTrace], relocated: &[FieldRelocation]) {
    let base_rel = relocated.iter().find(|r| r.field == "baseSelector");
    for trace in traces {
        let field_key = format!("fields.{}", trace.field_name);
        let rel = relocated.iter().find(|r| r.field == field_key).or(base_rel);
        if let (
            Some(rel),
            ExtractionMethod::CssSelector {
                relocated_from,
                step,
                ..
            },
        ) = (rel, &mut trace.method)
        {
            *relocated_from = Some(rel.old.clone());
            *step = Some(rel.step.to_owned());
        }
    }
}

pub(crate) fn to_css_traces(
    raw: Vec<EngineTrace>,
    relocated: &[FieldRelocation],
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    let mut traces = build_selector_traces(raw, source_url, |sel| ExtractionMethod::CssSelector {
        selector: sel,
        relocated_from: None,
        step: None,
    });
    if !relocated.is_empty() {
        apply_relocations(&mut traces, relocated);
    }
    traces
}

pub(crate) fn to_xpath_traces(
    raw: Vec<EngineTrace>,
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    build_selector_traces(raw, source_url, |selector| {
        ExtractionMethod::XPathSelector { selector }
    })
}

fn build_selector_traces(
    raw: Vec<EngineTrace>,
    source_url: Option<&str>,
    mut make_method: impl FnMut(String) -> ExtractionMethod,
) -> Vec<RawFieldTrace> {
    raw.into_iter()
        .map(|t| RawFieldTrace {
            field_path: t.field_path,
            field_name: t.field_name,
            value: t.value,
            raw_quote: t.raw_quote.unwrap_or_default(),
            attr_name: t.attr_name,
            method: make_method(t.raw_selector.unwrap_or_default()),
            source_url: source_url.map(str::to_owned),
            visible_in_dom: true,
        })
        .collect()
}

/// Convert extracted structured metadata (`json-ld`, `microdata`, `opengraph`) into flat evidence traces.
pub fn extract_structured_traces(
    data: &StructuredData,
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    let mut traces = Vec::new();
    for (idx, item) in data.json_ld.iter().enumerate() {
        let method = ExtractionMethod::JsonLd {
            entity_type: item
                .get("@type")
                .and_then(|v| {
                    v.as_str()
                        .or_else(|| v.as_array().and_then(|a| a.first()?.as_str()))
                })
                .map(str::to_owned),
            entity_id: item.get("@id").and_then(Value::as_str).map(str::to_owned),
        };
        collect_json_traces(
            item,
            &format!("/json-ld/{idx}"),
            &method,
            false,
            source_url,
            &mut traces,
        );
    }
    for (idx, item) in data.microdata.iter().enumerate() {
        let method = ExtractionMethod::Microdata {
            itemtype: item.get("type").and_then(Value::as_str).map(str::to_owned),
        };
        let target = item.get("properties").unwrap_or(item);
        collect_json_traces(
            target,
            &format!("/microdata/{idx}"),
            &method,
            true,
            source_url,
            &mut traces,
        );
    }
    for (prop, val) in &data.opengraph {
        let method = ExtractionMethod::OpenGraph {
            property: prop.clone(),
        };
        collect_json_traces(
            val,
            &format!("/opengraph/{prop}"),
            &method,
            false,
            source_url,
            &mut traces,
        );
    }
    traces.retain(|t| t.field_name != "@context");
    traces
}

/// Parse HTML for `json-ld`, `microdata`, and `opengraph` along with structured evidence traces.
pub fn extract_structured_with_evidence(
    html_str: &str,
    source_url: Option<&str>,
) -> StructuredWithEvidence {
    let data = extract_structured(html_str);
    let traces = extract_structured_traces(&data, source_url);
    StructuredWithEvidence { data, traces }
}

pub(crate) fn build_regex_traces(
    extractor: &RegexExtractor,
    out: &RegexOutput,
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    out.matches
        .iter()
        .enumerate()
        .map(|(idx, m)| RawFieldTrace {
            field_path: format!("/regex/{}/{}", m.label, idx),
            field_name: m.label.clone(),
            value: Value::String(m.value.clone()),
            raw_quote: m.value.clone(),
            attr_name: None,
            method: ExtractionMethod::RegexPattern {
                pattern: extractor
                    .pattern_for(&m.label)
                    .unwrap_or(&m.label)
                    .to_owned(),
                label: Some(m.label.clone()),
            },
            source_url: source_url.map(str::to_owned),
            visible_in_dom: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::InMemory;
    use crate::patterns::BuiltinPattern;
    use crate::xpath::JsonXPathExtractor;
    use serde_json::json;

    #[test]
    fn css_and_xpath_emit_aligned_traces() {
        let schema = Schema::from_json(&json!({
            "baseSelector": "div.p",
            "baseFields": [{"name": "id", "type": "attribute", "attribute": "data-id"}],
            "fields": [
                {"name": "title", "selector": "h2", "type": "text"},
                {"name": "price", "selector": ".price", "type": "regex", "pattern": "([\\d,.]+)"},
                {"name": "meta", "selector": ".m", "type": "nested", "fields": [{"name": "brand", "selector": "i", "type": "text"}]},
                {"name": "colors", "selector": "li", "type": "list", "fields": [{"name": "c", "type": "text"}]}
            ]
        }))
        .unwrap();
        let html = r#"<div class="p" data-id="7"><h2>Pro</h2><span class="price">$1,299.00</span><span class="m"><i>ACME</i></span><ul><li>red</li></ul></div>"#;
        let css = JsonCssExtractor::new(&schema).unwrap();
        let (items, traces) = css.extract_with_evidence(html, Some("https://example.com"));
        assert_eq!(items.len(), 1);
        let id_trace = traces.iter().find(|t| t.field_path == "/0/id").unwrap();
        assert_eq!(id_trace.attr_name.as_deref(), Some("data-id"));
        assert_eq!(id_trace.raw_quote, "7");
        let price_trace = traces.iter().find(|t| t.field_path == "/0/price").unwrap();
        assert_eq!(price_trace.value, json!("1,299.00"));
        assert_eq!(price_trace.raw_quote, "$1,299.00");
        assert!(
            traces
                .iter()
                .any(|t| t.field_path == "/0/meta/brand" && t.value == json!("ACME"))
        );
        assert!(
            traces
                .iter()
                .any(|t| t.field_path == "/0/colors/0/c" && t.value == json!("red"))
        );
        assert_eq!(items, css.extract(html));

        let xp_schema = Schema::from_json(&json!({
            "baseSelector": "//div[@class='p']",
            "fields": [{"name": "title", "selector": ".//h2", "type": "text"}]
        }))
        .unwrap();
        let xp = JsonXPathExtractor::new(&xp_schema).unwrap();
        let (xp_items, xp_traces) = xp.extract_with_evidence(html, None).unwrap();
        assert_eq!(xp_items[0]["title"], "Pro");
        assert!(matches!(
            xp_traces[0].method,
            ExtractionMethod::XPathSelector { ref selector } if selector == ".//h2"
        ));
    }

    #[test]
    fn adaptive_and_structured_and_regex_traces() {
        let schema = Schema::from_json(&json!({
            "baseSelector": "li.product",
            "fields": [
                {"name": "title", "selector": "h3.name", "type": "text"},
                {"name": "price", "selector": "span.price", "type": "text"}
            ]
        }))
        .unwrap();
        let mem = InMemory::default();
        let chrome = |body: String| {
            let nav: String = (0..10)
                .map(|i| format!("<a href='/c{i}'>Category {i}</a>"))
                .collect();
            let foot: String = (0..10)
                .map(|i| format!("<p>Footer note {i} about shipping</p>"))
                .collect();
            format!("<header><nav>{nav}</nav></header><main>{body}</main><footer>{foot}</footer>")
        };
        let old_html = chrome(
            r#"<ul>
            <li class="product"><h3 class="name">Alpha widget</h3><span class="price">$10</span></li>
            <li class="product"><h3 class="name">Beta gadget</h3><span class="price">$20</span></li>
            <li class="product"><h3 class="name">Gamma tool</h3><span class="price">$30</span></li>
        </ul>"#
                .into(),
        );
        extract_adaptive_with_evidence(&Html::parse(&old_html), &schema, &mem, None).unwrap();
        let new_html = chrome(
            r#"<div class="grid">
            <div class="css-1x8d"><h4 class="sc-a3b9">Alpha widget</h4><b class="sc-q9z2">$10</b></div>
            <div class="css-1x8d"><h4 class="sc-a3b9">Beta gadget</h4><b class="sc-q9z2">$20</b></div>
            <div class="css-1x8d"><h4 class="sc-a3b9">Gamma tool</h4><b class="sc-q9z2">$30</b></div>
        </div>"#
                .into(),
        );
        let res = extract_adaptive_with_evidence(
            &Html::parse(&new_html),
            &schema,
            &mem,
            Some("https://example.com"),
        )
        .unwrap();
        assert_eq!(res.outcome.items.len(), 3);
        assert!(res.traces.iter().any(|t| matches!(
            &t.method,
            ExtractionMethod::CssSelector { relocated_from: Some(old), step: Some(_), .. } if old == "h3.name" || old == "li.product"
        )));

        let structured_html = r##"<html><head>
            <meta property="og:title" content="Widget">
            <script type="application/ld+json">{"@context":"https://schema.org","@type":"Product","@id":"#prod","sku":"W1"}</script>
        </head><body><div itemscope itemtype="http://schema.org/Product"><span itemprop="name">Widget</span></div></body></html>"##;
        let sw = extract_structured_with_evidence(structured_html, Some("https://example.com"));
        assert!(
            sw.traces
                .iter()
                .any(|t| matches!(t.method, ExtractionMethod::JsonLd { .. })
                    && t.field_name == "sku"
                    && !t.visible_in_dom)
        );
        assert!(
            sw.traces
                .iter()
                .any(|t| matches!(t.method, ExtractionMethod::Microdata { .. })
                    && t.field_name == "name"
                    && t.visible_in_dom)
        );
        assert!(sw.traces.iter().any(|t| matches!(t.method, ExtractionMethod::OpenGraph { ref property } if property == "og:title")));

        let re = RegexExtractor::new::<&str, &str>(&[BuiltinPattern::Email], &[]).unwrap();
        let (out, re_traces) = re.extract_with_evidence("Contact sales@example.com today", None);
        assert_eq!(out.unique.len(), 1);
        assert_eq!(re_traces[0].value, json!("sales@example.com"));
        assert!(
            matches!(re_traces[0].method, ExtractionMethod::RegexPattern { ref label, .. } if label.as_deref() == Some("email"))
        );
    }
}
