use pagesift_html::{Html, Selector};

use super::*;

fn first<'a>(doc: &'a Html<'a>, sel: &str) -> NodeRef<'a> {
    doc.select(&Selector::parse(sel).unwrap()).next().unwrap()
}

const OLD: &str = "<main><nav><a href='/'>Home</a><a href='/shop'>Shop</a></nav>\
    <article class='product'><h1 class='title'>Blue Widget</h1>\
    <span class='price' data-sku='A1'>$19.99</span><p class='desc'>A sturdy widget for daily use.</p></article>\
    <footer><p>Contact us</p><p>About</p></footer></main>";

#[test]
fn exact_then_fingerprint_after_redesign() {
    let old = Html::parse(OLD);
    let t = Target::capture(old.root(), "span.price", None).unwrap();
    assert!(matches!(locate(old.root(), &t), Located::Exact(_)));
    // 改版：类名换成哈希、加一层包裹、标签换成 div
    let new = Html::parse(
        "<main><nav><a href='/'>Home</a><a href='/shop'>Shop</a></nav>\
         <article class='product'><div class='x'><h1 class='css-1x8dfa'>Blue Widget</h1></div>\
         <div class='sc-a3b9' data-sku='A1'>$19.99</div><p class='desc'>A sturdy widget for daily use.</p></article>\
         <footer><p>Contact us</p><p>About</p></footer></main>",
    );
    let got = locate(new.root(), &t);
    let node = got.node().unwrap_or_else(|| panic!("{got:?}"));
    assert_eq!(node.text(), "$19.99");
    let sel = robust_selector(node, new.root()).unwrap();
    assert!(sel.contains("data-sku"), "{sel}");
}

#[test]
fn identity_mismatch_is_missing_not_a_guess() {
    let old = Html::parse(OLD);
    let t = Target::capture(old.root(), "span.price", Some("data-sku")).unwrap();
    let swapped_src = OLD.replace("data-sku='A1'", "data-sku='B2'");
    let swapped = Html::parse(&swapped_src);
    assert_eq!(locate(swapped.root(), &t), Located::Missing);
    let moved_src = OLD.replace("class='price'", "class='cost'");
    let moved = Html::parse(&moved_src);
    assert!(matches!(locate(moved.root(), &t), Located::Anchored(_)));
}

#[test]
fn ties_are_ambiguous() {
    let old = Html::parse("<ul><li><b>Item</b></li></ul>");
    let t = Target::capture(old.root(), "b", None).unwrap();
    let new = Html::parse("<ol><li><i>Item</i></li><li><i>Item</i></li></ol>");
    assert!(matches!(locate(new.root(), &t), Located::Ambiguous { .. }));
}

#[test]
fn find_similar_climbs_to_list_items() {
    let doc = Html::parse(
        "<div id='list'><div class='card'><span class='p'>1</span><a>x</a></div>\
         <div class='card'><span class='p'>2</span><a>y</a></div>\
         <div class='card'><span class='p'>3</span><a>z</a></div></div><p>other</p>",
    );
    let price = first(&doc, "span.p");
    let s = find_similar(price, doc.root()).unwrap();
    assert_eq!(s.items.len(), 3);
    assert_eq!(s.depth, 1);
    let sel = s.selector.unwrap();
    assert_eq!(
        doc.select(&Selector::parse(&sel).unwrap()).count(),
        3,
        "{sel}"
    );
}
