//! `:nth-*` 位置缓存的差分测试：`select()`（一次遍历里复用缓存）和逐个元素调用
//! `matches()`（每次新建上下文、没有缓存可用）结果必须完全一致。

use pagesift_html::{Html, Selector};

/// 兄弟里混着不同标签、文本、注释、嵌套，覆盖 of-type、of S、从后往前数。
fn doc() -> String {
    let mut s = String::from("<div id=r>");
    for i in 0..60 {
        match i % 7 {
            0 => s.push_str("<p class=a>x</p>"),
            1 => s.push_str(" text <!-- c -->"),
            2 => s.push_str("<span>y</span>"),
            3 => s.push_str("<p>z<b>1</b><b class=a>2</b><i>3</i><b>4</b></p>"),
            4 => s.push_str("<x-el class=a></x-el>"),
            5 => s.push_str("<ul><li>a<li class=a>b<li>c<li class=a>d<li>e</ul>"),
            _ => s.push_str("<p class=a></p><span class=a></span>"),
        }
    }
    s.push_str("</div><p>tail</p>");
    s
}

#[test]
fn cached_select_equals_uncached_matches() {
    let html = doc();
    let doc = Html::parse(&html);
    let sels = [
        ":nth-child(2n+1)",
        ":nth-child(3)",
        ":nth-child(-n+4)",
        ":nth-last-child(2n)",
        ":nth-last-child(1)",
        "p:nth-of-type(2n+1)",
        "b:nth-of-type(2)",
        "x-el:nth-of-type(odd)",
        ":nth-last-of-type(1)",
        "span:nth-last-of-type(even)",
        ":nth-child(2n of .a)",
        ":nth-last-child(1 of .a)",
        "li:nth-child(odd) , p:nth-last-of-type(3n+2)",
        ":first-child",
        ":last-child",
        ":only-of-type",
        "div > :nth-child(5n+3)",
        ":nth-child(2n+1) :nth-child(2n+1)",
        ":has(> :nth-child(3))",
    ];
    let mut bad = Vec::new();
    for s in sels {
        let sel = Selector::parse(s).unwrap();
        let fast: Vec<_> = doc.select(&sel).map(|n| n.id()).collect();
        let slow: Vec<_> = doc
            .root()
            .descendants()
            .filter(|n| sel.matches(*n))
            .map(|n| n.id())
            .collect();
        if fast != slow {
            bad.push((s, fast.len(), slow.len()));
        }
        // 从子树开始选也一样
        let r = doc.select(&Selector::parse("#r").unwrap()).next().unwrap();
        let fast: Vec<_> = r.select(&sel).map(|n| n.id()).collect();
        let slow: Vec<_> = r
            .descendants()
            .skip(1)
            .filter(|n| sel.matches_with_scope(*n, Some(r)))
            .map(|n| n.id())
            .collect();
        if fast != slow {
            bad.push((s, fast.len(), slow.len()));
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// 模糊测试找到的：7.6 万个兄弟配 `:nth-child` 曾经要 6 秒（平方复杂度）。
#[test]
fn many_siblings_nth_is_linear() {
    let html = format!("<ul>{}</ul>", "<li>".repeat(80_000));
    let doc = Html::parse(&html);
    let t = std::time::Instant::now();
    for s in [
        "ul li:nth-child(2n+1)",
        "li:nth-last-child(3)",
        "li:nth-of-type(7n)",
    ] {
        let sel = Selector::parse(s).unwrap();
        assert!(doc.select(&sel).count() > 0);
    }
    assert!(
        t.elapsed() < std::time::Duration::from_secs(1),
        "{:?}",
        t.elapsed()
    );
}
