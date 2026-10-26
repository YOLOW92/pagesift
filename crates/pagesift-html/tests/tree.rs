//! 树构建的行为测试：把树转成缩进格式（类似 html5lib-tests），和预期逐行比较。

use pagesift_html::{Html, NodeRef};

fn dump(html: &str) -> String {
    let doc = Html::parse(html);
    let mut out = String::new();
    for child in doc.root().children() {
        walk(child, 0, &mut out);
    }
    out
}

fn walk(n: NodeRef<'_>, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    if let Some(el) = n.as_element() {
        out.push_str(&format!("{pad}<{}", el.name()));
        for (k, v) in el.attrs() {
            out.push_str(&format!(" {k}=\"{v}\""));
        }
        out.push_str(">\n");
    } else if let Some(t) = n.as_text() {
        out.push_str(&format!("{pad}\"{t}\"\n"));
    } else if let Some(c) = n.as_comment() {
        out.push_str(&format!("{pad}<!-- {c} -->\n"));
    } else {
        out.push_str(&format!("{pad}<!DOCTYPE>\n"));
    }
    for c in n.children() {
        walk(c, depth + 1, out);
    }
}

fn check(html: &str, expected: &str) {
    let expected: String = expected
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("{}\n", &l[4.min(l.len())..]))
        .collect();
    let got = dump(html);
    assert_eq!(got, expected, "\ninput: {html:?}\n--- got ---\n{got}");
}

#[test]
fn implied_structure() {
    check(
        "Hello",
        r#"
    <html>
      <head>
      <body>
        "Hello"
    "#,
    );
    check(
        "<!DOCTYPE html><title>T</title><meta charset=utf-8><p>x",
        r#"
    <!DOCTYPE>
    <html>
      <head>
        <title>
          "T"
        <meta charset="utf-8">
      <body>
        <p>
          "x"
    "#,
    );
}

#[test]
fn head_elements_after_head_go_into_head() {
    check(
        "<html><head></head><link rel=x><body>b",
        r#"
    <html>
      <head>
        <link rel="x">
      <body>
        "b"
    "#,
    );
}

#[test]
fn bom_and_crlf() {
    check(
        "\u{feff}<title>a</title>\r\nb\rc",
        r#"
    <html>
      <head>
        <title>
          "a"
      <body>
        "b
    c"
    "#,
    );
}

#[test]
fn paragraphs_and_lists_close_implicitly() {
    check(
        "<p>one<p>two<div>three</div><ul><li>a<li>b<ul><li>c</ul><li>d</ul>",
        r#"
    <html>
      <head>
      <body>
        <p>
          "one"
        <p>
          "two"
        <div>
          "three"
        <ul>
          <li>
            "a"
          <li>
            "b"
            <ul>
              <li>
                "c"
          <li>
            "d"
    "#,
    );
    check(
        "<dl><dt>t<dd>d<dt>u</dl>",
        r#"
    <html>
      <head>
      <body>
        <dl>
          <dt>
            "t"
          <dd>
            "d"
          <dt>
            "u"
    "#,
    );
}

#[test]
fn end_tags_respect_scope() {
    // `</div>` 在单元格里关不到表格外面的 div；无主的结束标签被忽略，前后文字合并
    check(
        "<div><table><tr><td>x</div>y</td></tr></table>z</div>w",
        r#"
    <html>
      <head>
      <body>
        <div>
          <table>
            <tbody>
              <tr>
                <td>
                  "xy"
          "z"
        "w"
    "#,
    );
    check(
        "<b>a</i>b</b>c",
        r#"
    <html>
      <head>
      <body>
        <b>
          "ab"
        "c"
    "#,
    );
}

#[test]
fn tables_get_tbody_and_foster_parenting() {
    check(
        "<table>stray<td>a<td>b<tr><th>c</table>",
        r#"
    <html>
      <head>
      <body>
        "stray"
        <table>
          <tbody>
            <tr>
              <td>
                "a"
              <td>
                "b"
            <tr>
              <th>
                "c"
    "#,
    );
    check(
        "<table><thead><tr><td>h<tbody><tr><td>b</table>",
        r#"
    <html>
      <head>
      <body>
        <table>
          <thead>
            <tr>
              <td>
                "h"
          <tbody>
            <tr>
              <td>
                "b"
    "#,
    );
    check(
        "<table><div>d</div><tr><td>x</td></tr></table>",
        r#"
    <html>
      <head>
      <body>
        <div>
          "d"
        <table>
          <tbody>
            <tr>
              <td>
                "x"
    "#,
    );
}

#[test]
fn raw_text_and_rcdata() {
    check(
        "<script>if (a < b && c) document.write('</p>')</script><style>p{}</style><textarea>\n&lt;x&gt;</textarea><title>a&amp;b</title>",
        r#"
    <html>
      <head>
        <script>
          "if (a < b && c) document.write('</p>')"
        <style>
          "p{}"
      <body>
        <textarea>
          "<x>"
        <title>
          "a&b"
    "#,
    );
    check(
        "<SCRIPT>x</SCRIPT >y",
        r#"
    <html>
      <head>
        <script>
          "x"
      <body>
        "y"
    "#,
    );
}

#[test]
fn pre_drops_first_newline() {
    check(
        "<pre>\n\ncode</pre>",
        r#"
    <html>
      <head>
      <body>
        <pre>
          "
    code"
    "#,
    );
}

#[test]
fn foreign_content() {
    check(
        "<svg viewBox='0 0 1 1'><path d=x /><title>t</title></svg><p>after",
        r#"
    <html>
      <head>
      <body>
        <svg viewbox="0 0 1 1">
          <path d="x">
          <title>
            "t"
        <p>
          "after"
    "#,
    );
    // 没闭合的 svg 里出现 HTML 块级标签：跳出外部内容
    check(
        "<svg><g><div>text</div>",
        r#"
    <html>
      <head>
      <body>
        <svg>
          <g>
        <div>
          "text"
    "#,
    );
}

#[test]
fn attributes() {
    check(
        "<a HREF='/x?a=1&amp;b=2&copy=3' href=dup data-x=\"q'\" checked>l</a>",
        r#"
    <html>
      <head>
      <body>
        <a href="/x?a=1&b=2&copy=3" data-x="q'" checked="">
          "l"
    "#,
    );
}

#[test]
fn comments_and_bogus_markup() {
    check(
        "<!-- c --><p>a<!---->b<?php x ?>c</p><!-->d<! x>e< f</>g",
        r#"
    <!--  c  -->
    <html>
      <head>
      <body>
        <p>
          "a"
          <!--  -->
          "b"
          <!-- ?php x ? -->
          "c"
        <!--  -->
        "d"
        <!--  x -->
        "e< fg"
    "#,
    );
}

#[test]
fn unclosed_tag_at_eof_is_dropped() {
    check(
        "<p>text<a href='x",
        r#"
    <html>
      <head>
      <body>
        <p>
          "text"
    "#,
    );
}

#[test]
fn deep_nesting_is_bounded() {
    let html = "<div>".repeat(100_000) + "deep";
    let doc = Html::parse(&html);
    let depth = doc
        .root()
        .descendants()
        .map(|n| n.ancestors().count())
        .max()
        .unwrap();
    assert!(depth <= 520, "depth {depth}");
    assert!(doc.root().text().contains("deep"));
}

#[test]
fn garbage_never_panics() {
    let samples = [
        "",
        "<",
        "</",
        "<!",
        "<!-",
        "<!--",
        "<a",
        "<a b",
        "<a b=",
        "<a b='",
        "&",
        "&#",
        "&#x",
        "&#x110000;",
        "<table><tr><td><table><td>",
        "<svg><![CDATA[x",
        "<math><mi><p>",
        "</body></html><p>after",
        "<frameset><frame>",
        "<select><option>a<option>b</select>",
        "<ruby>a<rt>b<rp>c</ruby>",
        "<template><td>x</template>",
        "<plaintext><p>x",
    ];
    for s in samples {
        let doc = Html::parse(s);
        let _ = doc.root().descendants().count();
    }
}

/// 找第一个名为 `name` 的元素，返回它的全部属性。
fn attrs_of(html: &str, name: &str) -> Vec<(String, String)> {
    fn find(n: NodeRef<'_>, name: &str) -> Option<Vec<(String, String)>> {
        if let Some(el) = n.as_element()
            && el.name() == name
        {
            return Some(
                el.attrs()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            );
        }
        n.children().find_map(|c| find(c, name))
    }
    let doc = Html::parse(html);
    doc.root().children().find_map(|c| find(c, name)).unwrap()
}

#[test]
fn duplicate_attributes_keep_first() {
    let got = attrs_of(r#"<div a="1" B="2" A="3" b="4" c="5">x</div>"#, "div");
    let want = [("a", "1"), ("b", "2"), ("c", "5")];
    assert_eq!(
        got,
        want.map(|(k, v)| (k.to_string(), v.to_string())).to_vec()
    );
}

#[test]
fn duplicate_attributes_keep_first_past_linear_limit() {
    // 超过线性比较上限后改走哈希集合，语义必须不变
    let mut html = String::from("<div");
    for i in 0..40 {
        html.push_str(&format!(" a{i}=\"first{i}\""));
    }
    for i in 0..40 {
        html.push_str(&format!(" A{i}=\"second{i}\""));
    }
    html.push_str(">x</div>");
    let got = attrs_of(&html, "div");
    assert_eq!(got.len(), 40);
    for (i, (k, v)) in got.iter().enumerate() {
        assert_eq!(
            (k.as_str(), v.as_str()),
            (&*format!("a{i}"), &*format!("first{i}"))
        );
    }
}

#[test]
fn many_distinct_attributes_parse_quickly() {
    // 平方复杂度时 5 万个属性要几十秒；这里只要求能跑完（测试超时即视为回归）
    let n = 50_000;
    let mut html = String::from("<div");
    for i in 0..n {
        html.push_str(&format!(" x{i}=1"));
    }
    html.push_str(" x0=dup>x</div>");
    assert_eq!(attrs_of(&html, "div").len(), n);
}

#[test]
fn repeated_body_merges_many_attributes() {
    // 第二个 <body> 的属性补到第一个上：两边都很长时也不能平方
    let n = 20_000;
    let mut html = String::from("<body");
    for i in 0..n {
        html.push_str(&format!(" a{i}=1"));
    }
    html.push_str("><body a0=2");
    for i in 0..n {
        html.push_str(&format!(" b{i}=1"));
    }
    html.push_str(">x");
    let got = attrs_of(&html, "body");
    assert_eq!(got.len(), 2 * n);
    assert_eq!(got[0], ("a0".to_string(), "1".to_string()));
}
