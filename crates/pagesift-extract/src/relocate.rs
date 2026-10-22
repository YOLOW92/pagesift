//! Serializable anchors for relocation across DOM changes. Business identity is
//! mandatory: visual similarity cannot establish that an item is still present.
use pagesift_html::{Html, NodeRef, Selector};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Anchor {
    selector: String,
    identity_attribute: String,
    identity: String,
    tag: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Relocation<'a> {
    Exact(NodeRef<'a>),
    Relocated(NodeRef<'a>),
    Missing,
    Ambiguous,
}

impl Anchor {
    /// Capture exactly one element with a nonempty stable business attribute
    /// (for example data-product-id). DOM id and text are deliberately not guessed.
    pub fn capture(
        doc: &Html<'_>,
        selector: &str,
        identity_attribute: &str,
    ) -> Result<Self, String> {
        let sel = Selector::parse(selector).map_err(|e| e.to_string())?;
        let nodes: Vec<_> = doc.select(&sel).collect();
        if nodes.len() != 1 {
            return Err("anchor selector must match exactly one element".into());
        }
        let el = nodes[0].as_element().ok_or("anchor is not an element")?;
        let identity = el
            .attr(identity_attribute)
            .filter(|s| !s.trim().is_empty())
            .ok_or("stable business identity is required")?;
        Ok(Self {
            selector: selector.into(),
            identity_attribute: identity_attribute.into(),
            identity: identity.into(),
            tag: el.name().into(),
        })
    }

    /// Search the entire document, including when an old selector still matches
    /// a different item. Duplicate identities are always rejected, even if only
    /// one duplicate retains the original selector.
    pub fn locate<'a>(&self, doc: &'a Html<'a>) -> Relocation<'a> {
        let mut matches = doc.root().descendants().filter(|n| {
            n.as_element()
                .is_some_and(|e| e.attr(&self.identity_attribute) == Some(self.identity.as_str()))
        });
        let Some(node) = matches.next() else {
            return Relocation::Missing;
        };
        if matches.next().is_some() {
            return Relocation::Ambiguous;
        }
        if node.as_element().is_some_and(|e| e.name() == self.tag)
            && Selector::parse(&self.selector).is_ok_and(|s| s.matches(node))
        {
            Relocation::Exact(node)
        } else {
            Relocation::Relocated(node)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_reused_selectors_duplicates_and_missing_identity() {
        let old = Html::parse("<div class='old' data-sku='A'>Old price</div>");
        let anchor = Anchor::capture(&old, ".old", "data-sku").unwrap();
        let anchor: Anchor =
            serde_json::from_str(&serde_json::to_string(&anchor).unwrap()).unwrap();
        let moved = Html::parse(
            "<div class='old' data-sku='B'>Old price</div><main><div data-sku='A'>New price</div></main>",
        );
        assert!(
            matches!(anchor.locate(&moved), Relocation::Relocated(n) if n.text() == "New price")
        );
        let duplicate =
            Html::parse("<div class='old' data-sku='A'>1</div><div data-sku='A'>2</div>");
        assert!(matches!(anchor.locate(&duplicate), Relocation::Ambiguous));
        let absent = Html::parse("<div class='old' data-sku='B'>Old price</div>");
        assert!(matches!(anchor.locate(&absent), Relocation::Missing));
        assert!(Anchor::capture(&old, ".old", "data-missing").is_err());
    }
    #[test]
    fn tag_changes_relocate_but_cross_tag_duplicates_are_ambiguous() {
        let old = Html::parse("<div class='card' data-key='A'>Old</div>");
        let anchor = Anchor::capture(&old, ".card", "data-key").unwrap();
        let changed = Html::parse("<article class='card' data-key='A'>Updated</article>");
        assert!(
            matches!(anchor.locate(&changed), Relocation::Relocated(n) if n.text() == "Updated")
        );
        let duplicate = Html::parse(
            "<div class='card' data-key='A'>Old</div><article data-key='A'>Other</article>",
        );
        assert!(matches!(anchor.locate(&duplicate), Relocation::Ambiguous));
    }
}
