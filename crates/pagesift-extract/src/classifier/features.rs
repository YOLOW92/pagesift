//! 毫秒级 DOM 结构与语义特征提取器，实现快速多品类页面识别。

use pagesift_html::{Html, Tag};
use super::page_type::PageType;

/// 快速提取页面结构特征并判定品类类型（耗时 < 0.1ms）。
pub fn classify(doc: &Html<'_>, url: Option<&str>) -> PageType {
    // 1. URL 强提示判别
    if let Some(u) = url {
        let u_low = u.to_lowercase();
        if u_low.contains("/product/") || u_low.contains("/products/") || u_low.contains("/item/") || u_low.contains("/p/") {
            return PageType::Product;
        }
        if u_low.contains("/forum/") || u_low.contains("/thread/") || u_low.contains("/t/") || u_low.contains("/topic/") || u_low.contains("/discuss/") {
            return PageType::Forum;
        }
        if u_low.contains("/docs/") || u_low.contains("/doc/") || u_low.contains("/guide/") || u_low.contains("/reference/") || u_low.contains("/api/") {
            return PageType::Documentation;
        }
    }

    let mut product_score = 0;
    let mut forum_score = 0;
    let mut doc_score = 0;
    let mut article_score = 0;
    let mut listing_score = 0;

    let mut total_text_len = 0;
    let mut total_link_text_len = 0;
    let mut p_count = 0;
    let mut code_blocks = 0;
    let mut item_cards = 0;

    for node in doc.root().descendants() {
        let Some(el) = node.as_element() else {
            continue;
        };

        match el.tag() {
            Tag::P => {
                p_count += 1;
                total_text_len += node.text().len();
            }
            Tag::A => {
                total_link_text_len += node.text().len();
            }
            Tag::Pre => {
                code_blocks += 1;
            }
            Tag::Article => {
                article_score += 3;
            }
            _ => {}
        }

        // 检查 class / id 语义
        for val in [el.attr("class"), el.attr("id"), el.attr("itemtype")].into_iter().flatten() {
            let low = val.to_lowercase();
            if low.contains("product") || low.contains("price") || low.contains("add-to-cart") || low.contains("addtocart") || low.contains("shopify") {
                product_score += 2;
            }
            if low.contains("discourse") || low.contains("forum") || low.contains("thread") || low.contains("topic-body") || low.contains("reply-count") {
                forum_score += 2;
            }
            if low.contains("api-doc") || low.contains("sidebar-nav") || low.contains("code-block") || low.contains("apireference") {
                doc_score += 2;
            }
            if low.contains("product-card") || low.contains("grid-item") || low.contains("result-item") {
                item_cards += 1;
            }
        }

        // 按钮文本特征
        if matches!(el.tag(), Tag::Button) {
            let btn_txt = node.text().to_lowercase();
            if btn_txt.contains("add to cart") || btn_txt.contains("buy now") || btn_txt.contains("checkout") {
                product_score += 5;
            }
            if btn_txt.contains("reply") || btn_txt.contains("new topic") {
                forum_score += 3;
            }
        }
    }

    if item_cards >= 6 {
        listing_score += 6;
    }

    if code_blocks >= 2 || doc_score >= 5 {
        return PageType::Documentation;
    }

    if product_score >= 5 && product_score > forum_score {
        return PageType::Product;
    }

    if forum_score >= 4 {
        return PageType::Forum;
    }

    if listing_score >= 6 && total_link_text_len * 2 >= total_text_len {
        return PageType::Listing;
    }

    if p_count >= 5 || article_score >= 3 {
        return PageType::Article;
    }

    PageType::General
}
