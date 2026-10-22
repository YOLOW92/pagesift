//! Conservative item discovery from explicit semantic identities. URLs, text and
//! DOM ids alone do not establish a business identity.
use pagesift_html::Html;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentifiedItem {
    pub id: String,
    pub identity_attribute: String,
    pub text: String,
    pub html: String,
}

/// Discover explicitly identified items without a site-specific CSS selector.
/// Reject conflicting attributes and duplicate identities instead of guessing.
pub fn discover_items(html: &str) -> Result<Vec<IdentifiedItem>, String> {
    let doc = Html::parse(html);
    let mut ids = BTreeSet::new();
    let mut out = Vec::new();
    for node in doc.root().descendants() {
        let Some(el) = node.as_element() else {
            continue;
        };
        let mut identity = None;
        for attr in ["data-item-id", "data-product-id", "itemid"] {
            if let Some(value) = el.attr(attr).filter(|v| !v.trim().is_empty()) {
                if identity.is_some_and(|(_, old)| old != value) {
                    return Err("conflicting business identities on an element".into());
                }
                identity = Some((attr, value));
            }
        }
        if let Some((attr, id)) = identity {
            if !ids.insert(id.to_owned()) {
                return Err(format!("duplicate business identity: {id}"));
            }
            out.push(IdentifiedItem {
                id: id.into(),
                identity_attribute: attr.into(),
                text: node.text(),
                html: node.html(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_discovery_rejects_ambiguity_and_ignores_generated_dom_ids() {
        let items = discover_items("<main><div id='random'>No identity</div><article data-product-id='SKU-東京'>Product</article></main>").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "SKU-東京");
        assert!(discover_items("<i data-item-id='A'></i><b itemid='A'></b>").is_err());
        assert!(discover_items("<i data-item-id='A' itemid='B'></i>").is_err());
    }
}
