//! 结构化数据提取门面模块。
//! 整合 Microdata、JSON-LD 与 OpenGraph 抽取，对齐 extruct 输出能力。

pub mod json_ld;
pub mod microdata;
pub mod opengraph;

use pagesift_html::Html;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use json_ld::{ProductInfo, extract_json_ld, extract_product_view};
pub use microdata::extract_microdata;
pub use opengraph::extract_opengraph;

/// 完整的页面结构化数据提取结果（对齐 extruct 数据模型）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructuredData {
    #[serde(rename = "json-ld")]
    pub json_ld: Vec<Value>,
    pub microdata: Vec<Value>,
    pub opengraph: Map<String, Value>,
}

/// 提取 HTML 文本中的所有结构化数据
pub fn extract_structured(html_str: &str) -> StructuredData {
    let doc = Html::parse(html_str);
    StructuredData {
        json_ld: extract_json_ld(&doc),
        microdata: extract_microdata(&doc),
        opengraph: extract_opengraph(&doc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microdata_product_extraction() {
        let html = r#"
        <div itemscope itemtype="http://schema.org/Product">
            <span itemprop="brand">ACME</span>
            <h1 itemprop="name">Executive Anvil</h1>
            <img itemprop="image" src="anvil.jpg" />
            <div itemprop="aggregateRating" itemscope itemtype="http://schema.org/AggregateRating">
                <span itemprop="ratingValue">4.4</span>
                <span itemprop="reviewCount">89</span>
            </div>
            <div itemprop="offers" itemscope itemtype="http://schema.org/Offer">
                <meta itemprop="priceCurrency" content="USD" />
                <span itemprop="price">119.99</span>
                <link itemprop="availability" href="http://schema.org/InStock" />
            </div>
        </div>
        "#;

        let res = extract_structured(html);
        assert_eq!(res.microdata.len(), 1);

        let prod = &res.microdata[0];
        assert_eq!(prod["type"], "http://schema.org/Product");
        assert_eq!(prod["properties"]["brand"], "ACME");
        assert_eq!(prod["properties"]["name"], "Executive Anvil");
        assert_eq!(prod["properties"]["image"], "anvil.jpg");
        assert_eq!(
            prod["properties"]["aggregateRating"]["properties"]["ratingValue"],
            "4.4"
        );
        assert_eq!(
            prod["properties"]["offers"]["properties"]["price"],
            "119.99"
        );
        assert_eq!(
            prod["properties"]["offers"]["properties"]["priceCurrency"],
            "USD"
        );
        assert_eq!(
            prod["properties"]["offers"]["properties"]["availability"],
            "http://schema.org/InStock"
        );
    }

    #[test]
    fn test_json_ld_graph_extraction() {
        let html = r#"
        <html><head>
        <script type="application/ld+json">
        {
            "@context": "https://schema.org",
            "@graph": [
                {
                    "@type": "Product",
                    "name": "Super Widget",
                    "sku": "WIDGET-01",
                    "offers": {
                        "@type": "Offer",
                        "price": "29.99",
                        "priceCurrency": "USD"
                    }
                },
                {
                    "@type": "Organization",
                    "name": "Widget Corp"
                }
            ]
        }
        </script>
        </head></html>
        "#;

        let res = extract_structured(html);
        assert_eq!(res.json_ld.len(), 2);

        let prod = extract_product_view(&res.json_ld).expect("product extracted");
        assert_eq!(prod.name.as_deref(), Some("Super Widget"));
        assert_eq!(prod.sku.as_deref(), Some("WIDGET-01"));
        assert_eq!(prod.price.as_deref(), Some("29.99"));
        assert_eq!(prod.currency.as_deref(), Some("USD"));
    }
}
