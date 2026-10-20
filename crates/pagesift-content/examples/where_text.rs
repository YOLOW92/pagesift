//! 诊断：短语出现在页面结构的哪里（文本节点的祖先链，带 id 和 class）。
//! `cargo run --release -p pagesift-content --example where_text -- page.html "短语" ["短语"...]`

use pagesift_html::Html;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: where_text <page.html> <phrase>...");
    let html = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    let doc = Html::parse(&html);
    for phrase in args {
        let needle = phrase.to_lowercase();
        let key: String = needle.chars().take(24).collect();
        let found = doc.root().descendants().find(|n| {
            n.as_text().is_some_and(|t| {
                t.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(&key)
            })
        });
        println!("== {phrase}");
        let Some(node) = found else {
            println!("   (not in a single text node)");
            continue;
        };
        let mut chain: Vec<String> = node
            .ancestors()
            .filter_map(|a| {
                let e = a.as_element()?;
                let mut s = e.name().to_string();
                if let Some(id) = e.attr("id") {
                    s.push('#');
                    s.push_str(id);
                }
                for c in e
                    .attr("class")
                    .unwrap_or_default()
                    .split_whitespace()
                    .take(3)
                {
                    s.push('.');
                    s.push_str(c);
                }
                if let Some(r) = e.attr("role") {
                    s.push_str(&format!("[role={r}]"));
                }
                Some(s)
            })
            .collect();
        chain.reverse();
        println!("   {}", chain.join(" > "));
    }
}
