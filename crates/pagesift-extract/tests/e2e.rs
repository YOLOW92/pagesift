//! 端到端：同一个页面分别用 CSS schema 和 XPath schema 提取，结果应一致。

use pagesift_extract::{JsonCssExtractor, JsonXPathExtractor, Schema};
use serde_json::{Value, json};

const SHOP: &str = r#"<!doctype html><html><head><title>Shop</title></head><body>
<nav><a href="/">Home</a></nav>
<div class="grid">
  <div class="product" data-sku="A-100">
    <a class="link" href="/p/a-100"><img src="/img/a.jpg" alt="Kettle"></a>
    <h3 class="name"> Electric   Kettle </h3>
    <span class="price">$39.99</span><span class="old-price">$49.99</span>
    <div class="rating" title="4.5 out of 5">★★★★½</div>
    <ul class="tags"><li>kitchen</li><li>sale</li></ul>
    <div class="seller"><span class="seller-name">ACME</span><span class="seller-country">US</span></div>
  </div>
  <div class="product" data-sku="B-200">
    <a class="link" href="/p/b-200"><img src="/img/b.jpg" alt="Toaster"></a>
    <h3 class="name">Toaster</h3>
    <span class="price">$1,299.00</span>
    <ul class="tags"></ul>
  </div>
</div>
<footer>© Shop</footer>
</body></html>"#;

const NEWS: &str = r#"<html><body><main>
<article class="story"><h2><a href="https://news.example/a">Rust 2.0 released</a></h2>
  <p class="meta"><span class="author">Ann</span> · <time datetime="2026-10-01T08:00:00Z">Oct 1</time></p>
  <p class="summary">The new edition brings faster builds.</p>
  <div class="comments">128 comments</div></article>
<article class="story"><h2><a href="https://news.example/b">网页抓取引擎发布</a></h2>
  <p class="meta"><span class="author">李雷</span> · <time datetime="2026-10-02T09:30:00Z">10月2日</time></p>
  <p class="summary">更快、更干净。</p></article>
<aside><article class="ad"><h2>Buy now</h2></article></aside>
</main></body></html>"#;

fn shop_expected() -> Vec<Value> {
    vec![
        json!({"sku": "A-100", "name": "electric kettle", "url": "/p/a-100", "image": "/img/a.jpg",
               "price": "39.99", "old_price": "$49.99", "rating": "4.5", "tags": [{"tag": "kitchen"}, {"tag": "sale"}],
               "seller": {"name": "ACME", "country": "US"}}),
        json!({"sku": "B-200", "name": "toaster", "url": "/p/b-200", "image": "/img/b.jpg",
               "price": "1,299.00", "rating": "n/a", "tags": [], "seller": {}}),
    ]
}

fn news_expected() -> Vec<Value> {
    vec![
        json!({"title": "Rust 2.0 released", "url": "https://news.example/a", "author": "Ann",
               "published": "2026-10-01T08:00:00Z", "summary": "The new edition brings faster builds.", "comments": "128"}),
        json!({"title": "网页抓取引擎发布", "url": "https://news.example/b", "author": "李雷",
               "published": "2026-10-02T09:30:00Z", "summary": "更快、更干净。", "comments": "0"}),
    ]
}

fn css(schema: Value, html: &str) -> Vec<Value> {
    JsonCssExtractor::new(&Schema::from_json(&schema).unwrap())
        .unwrap()
        .extract(html)
}

fn xpath(schema: Value, html: &str) -> Vec<Value> {
    JsonXPathExtractor::new(&Schema::from_json(&schema).unwrap())
        .unwrap()
        .extract(html)
        .unwrap()
}

#[test]
fn ecommerce_css() {
    let schema = json!({
        "name": "Products",
        "baseSelector": "div.product",
        "baseFields": [{"name": "sku", "type": "attribute", "attribute": "data-sku"}],
        "fields": [
            {"name": "name", "selector": "h3.name", "type": "text", "transform": "lowercase"},
            {"name": "url", "selector": "a.link", "type": "attribute", "attribute": "href"},
            {"name": "image", "selector": "img", "type": "attribute", "attribute": "src"},
            {"name": "price", "selector": ".price", "type": "regex", "pattern": "\\$([\\d,.]+)"},
            {"name": "old_price", "selector": ".old-price", "type": "text", "default": null},
            {"name": "rating", "selector": ".rating", "type": ["attribute", "regex"], "attribute": "title",
             "pattern": "([\\d.]+) out of", "default": "n/a"},
            {"name": "tags", "selector": "ul.tags li", "type": "list", "fields": [{"name": "tag", "type": "text"}]},
            {"name": "seller", "selector": ".seller", "type": "nested", "fields": [
                {"name": "name", "selector": ".seller-name", "type": "text"},
                {"name": "country", "selector": ".seller-country", "type": "text"}
            ]}
        ]
    });
    assert_eq!(css(schema, SHOP), shop_expected());
}

#[test]
fn ecommerce_xpath() {
    let schema = json!({
        "baseSelector": "//div[@class='product']",
        "baseFields": [{"name": "sku", "type": "attribute", "attribute": "data-sku"}],
        "fields": [
            {"name": "name", "selector": ".//h3[contains(@class, 'name')]", "type": "text", "transform": "lowercase"},
            {"name": "url", "selector": "./a[@class='link']", "type": "attribute", "attribute": "href"},
            {"name": "image", "selector": ".//img/@src", "type": "text"},
            {"name": "price", "selector": ".//span[@class='price']", "type": "regex", "pattern": "\\$([\\d,.]+)"},
            {"name": "old_price", "selector": ".//span[starts-with(@class, 'old')]", "type": "text", "default": null},
            {"name": "rating", "selector": ".//div[@class='rating']", "type": ["attribute", "regex"], "attribute": "title",
             "pattern": "([\\d.]+) out of", "default": "n/a"},
            {"name": "tags", "selector": ".//ul[@class='tags']/li", "type": "list", "fields": [{"name": "tag", "type": "text"}]},
            {"name": "seller", "selector": ".//div[@class='seller']", "type": "nested", "fields": [
                {"name": "name", "selector": "./span[1]", "type": "text"},
                {"name": "country", "selector": "./span[last()]", "type": "text"}
            ]}
        ]
    });
    assert_eq!(xpath(schema, SHOP), shop_expected());
}

#[test]
fn news_css() {
    let schema = json!({
        "baseSelector": "main > article.story",
        "fields": [
            {"name": "title", "selector": "h2 a", "type": "text"},
            {"name": "url", "selector": "h2 a", "type": "attribute", "attribute": "href"},
            {"name": "author", "selector": ".meta .author", "type": "text"},
            {"name": "published", "selector": "time", "type": "attribute", "attribute": "datetime"},
            {"name": "summary", "selector": "p.summary", "type": "text"},
            {"name": "comments", "selector": ".comments", "type": "regex", "pattern": "(\\d+)", "default": "0"}
        ]
    });
    assert_eq!(css(schema, NEWS), news_expected());
}

#[test]
fn news_xpath() {
    let schema = json!({
        "baseSelector": "//main/article[@class='story']",
        "fields": [
            {"name": "title", "selector": "./h2/a", "type": "text"},
            {"name": "url", "selector": "./h2/a", "type": "attribute", "attribute": "href"},
            {"name": "author", "selector": ".//span[@class='author']/text()", "type": "text"},
            {"name": "published", "selector": ".//time", "type": "attribute", "attribute": "datetime"},
            {"name": "summary", "selector": "./p[2]", "type": "text"},
            {"name": "comments", "selector": ".//div[@class='comments']", "type": "regex", "pattern": "(\\d+)", "default": "0"}
        ]
    });
    assert_eq!(xpath(schema, NEWS), news_expected());
}

#[test]
fn bad_schemas_return_errors() {
    let bad = [
        json!({"baseSelector": 3, "fields": []}),
        json!({"baseSelector": "div", "fields": {}}),
        json!({"baseSelector": "div", "fields": [{"name": "x"}]}),
        json!({"baseSelector": "div", "fields": [{"name": "x", "type": "regex"}]}),
        json!({"baseSelector": "div", "fields": [{"name": "x", "type": "nested_list", "selector": "a", "fields": [{"type": "text"}]}]}),
    ];
    for s in bad {
        let err = Schema::from_json(&s).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
    let s = Schema::from_json(&json!({"baseSelector": "div[", "fields": []})).unwrap();
    assert!(JsonCssExtractor::new(&s).is_err());
    assert!(JsonXPathExtractor::new(&s).is_err());
}
