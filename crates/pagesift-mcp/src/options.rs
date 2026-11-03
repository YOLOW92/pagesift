//! 抓取选项的参数表：按引擎能力生成，不手写格式和动作清单。
//!
//! 每个属性都对应 `pagesift_api::types::ScrapeOptions` 的一个真实字段（测试逐条核对），
//! `formats` 的取值就是引擎的 `supported_formats`，`actions` 的类型就是浏览器实际支持的动作；
//! 引擎不支持动作时不出现 `actions`。表外的字段一律拒收（`additionalProperties: false`）。

use serde_json::{Map, Value, json};

/// 引擎能做什么：参数表由它生成。
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// `Engine::supported_formats`。
    pub formats: Vec<String>,
    /// 浏览器支持的动作类型（`pagesift_cdp::SUPPORTED_ACTIONS`）；没有浏览器时为空。
    pub actions: Vec<String>,
    /// `Engine::supports_interact`：出现 `keepSession` 和 `pagesift_interact` 工具。
    pub interact: bool,
}

impl Capabilities {
    pub fn new(formats: &[&str], actions: &[&str]) -> Self {
        Self {
            formats: formats.iter().map(|s| s.to_string()).collect(),
            actions: actions.iter().map(|s| s.to_string()).collect(),
            interact: false,
        }
    }

    pub fn with_interact(mut self, on: bool) -> Self {
        self.interact = on;
        self
    }

    /// 不带浏览器、不带 LLM 和存储时的最小能力（占位工具、测试用）。
    pub fn basic() -> Self {
        Self::new(&["markdown", "html", "rawHtml", "links", "images"], &[])
    }
}

/// `ScrapeOptions` 里不暴露给 MCP 的字段，以及原因（测试要求表里每个字段要么暴露、要么在这里写明）。
pub const NOT_EXPOSED: &[(&str, &str)] = &[
    (
        "skipTlsVerification",
        "pagesift always verifies certificates; the flag has no effect",
    ),
    (
        "ignoreRobotsTxt",
        "not offered to agents: whether robots.txt may be ignored is the operator's decision in pagesift.toml",
    ),
];

/// 一个浏览器动作的参数表：抓取前的 `actions` 和 `pagesift_interact` 共用。
pub fn action_schema(actions: &[String]) -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": { "type": "string", "enum": actions },
            "selector": { "type": "string" },
            "all": { "type": "boolean" },
            "milliseconds": { "type": "integer", "minimum": 0 },
            "text": { "type": "string" },
            "key": { "type": "string" },
            "direction": { "type": "string", "enum": ["up", "down"] },
            "script": { "type": "string" },
            "fullPage": { "type": "boolean" },
            // [协议] JPEG 质量 1–100（CDP Page.captureScreenshot 的 quality）
            "quality": { "type": "integer", "minimum": 1, "maximum": 100 },
            "viewport": {
                "type": "object",
                "properties": { "width": { "type": "integer" }, "height": { "type": "integer" } },
                "required": ["width", "height"]
            },
            "format": { "type": "string" },
            "landscape": { "type": "boolean" },
            "scale": { "type": "number" }
        },
        "required": ["type"]
    })
}

fn str_array() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

/// 单页抓取选项（不含 `url`）。
pub fn scrape_options_schema(caps: &Capabilities) -> Value {
    let mut p = Map::new();
    let mut put = |k: &str, v: Value| {
        p.insert(k.to_string(), v);
    };
    let formats = Value::from(caps.formats.clone());
    put(
        "formats",
        json!({
            "type": "array",
            "items": {
                "anyOf": [
                    { "type": "string", "enum": formats },
                    {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": formats },
                            "schema": { "type": "object", "description": "extract: CSS schema {baseSelector, fields}; json: JSON Schema for the LLM." },
                            "prompt": { "type": "string", "description": "json: what to extract." }
                        },
                        "required": ["type"]
                    }
                ]
            },
            "description": "Output formats. Defaults to [\"markdown\"]. Object form passes options: {\"type\": \"extract\", \"schema\": {\"baseSelector\": ..., \"fields\": [...]}} extracts structured data with CSS selectors (no LLM; selectors are relocated after a redesign), {\"type\": \"json\", \"schema\": {...}, \"prompt\": \"...\"} asks the LLM."
        }),
    );
    put(
        "headers",
        json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Extra request headers." }),
    );
    put("includeTags", str_array());
    put("excludeTags", str_array());
    put(
        "onlyMainContent",
        json!({ "type": "boolean", "description": "Keep only the main content. Defaults to true." }),
    );
    put(
        "timeout",
        json!({ "type": "integer", "minimum": 1000, "description": "Timeout in milliseconds." }),
    );
    put(
        "waitFor",
        json!({ "type": "integer", "minimum": 0, "description": "Milliseconds to wait before capturing a rendered page." }),
    );
    put(
        "mobile",
        json!({ "type": "boolean", "description": "Emulate a phone when rendering." }),
    );
    if !caps.actions.is_empty() {
        put(
            "actions",
            json!({
                "type": "array",
                "items": action_schema(&caps.actions),
                "description": "Browser actions to run before capturing the page."
            }),
        );
    }
    if caps.interact {
        put(
            "keepSession",
            json!({
                "type": "boolean",
                "description": "Keep the browser page open and return its id in metadata.scrapeId, to continue with pagesift_interact. Only on pagesift_scrape. Close it with pagesift_interact_stop."
            }),
        );
    }
    put(
        "location",
        json!({
            "type": "object",
            "properties": { "country": { "type": "string" }, "languages": str_array() },
            "additionalProperties": false
        }),
    );
    put(
        "parsers",
        json!({ "type": "array", "description": "PDF parser selection, e.g. [\"pdf\"] or [{\"type\": \"pdf\", \"maxPages\": 10}]." }),
    );
    put("removeBase64Images", json!({ "type": "boolean" }));
    put(
        "blockAds",
        json!({ "type": "boolean", "description": "Block ads when rendering. Defaults to true." }),
    );
    put(
        "proxy",
        json!({
            "type": "string",
            "description": "\"basic\", \"auto\", or an http/https/socks5 proxy URL. pagesift does not solve CAPTCHAs; a block or challenge page is reported as an error."
        }),
    );
    put(
        "egress",
        json!({ "type": "string", "description": "Name of a configured [[egress]] to fetch through." }),
    );
    put(
        "maxAge",
        json!({ "type": "integer", "minimum": 0, "description": "Accept a cached copy at most this many milliseconds old; 0 forces a live fetch." }),
    );
    put(
        "minAge",
        json!({ "type": "integer", "minimum": 0, "description": "Only return a cached copy at least this many milliseconds old." }),
    );
    put("storeInCache", json!({ "type": "boolean" }));
    put(
        "engine",
        json!({
            "type": "string",
            "enum": ["http", "browser", "auto"],
            "description": "Fetch with plain HTTP, a headless browser, or decide automatically (default)."
        }),
    );
    put(
        "discoverScripts",
        json!({ "type": "boolean", "description": "With engine http and the links format: also list same-origin URLs found in external scripts." }),
    );
    put(
        "explain",
        json!({ "type": "boolean", "description": "Record how the result was produced in metadata.pagesift.plan." }),
    );
    put(
        "exportImages",
        json!({ "type": "boolean", "description": "Export the page's images (PDF embedded images, Office media, web images); returned as resource links." }),
    );
    put(
        "ifNoneMatch",
        json!({ "type": "string", "description": "ETag for a conditional request." }),
    );
    put(
        "ifModifiedSince",
        json!({ "type": "string", "description": "HTTP date for a conditional request." }),
    );
    put(
        "storeSnapshot",
        json!({ "type": "boolean", "description": "Keep a snapshot of this page for later change tracking." }),
    );
    put(
        "queryFilter",
        json!({
            "type": ["string", "object"],
            "description": "Keep only the blocks relevant to a query: a string, or {query, threshold, relative, topK, maxTokens, minTokens, headings}."
        }),
    );
    put(
        "respondTiming",
        json!({
            "type": "string",
            "enum": ["html", "visible-content", "mutation-idle", "resource-idle", "media-idle", "network-idle"],
            "description": "When a rendered page counts as ready: html does not run scripts; the others wait for visible content, DOM mutations to settle, scripts/XHR to go quiet, or media to load."
        }),
    );
    put(
        "chunking",
        json!({
            "type": "object",
            "properties": { "type": { "type": "string" } },
            "required": ["type"],
            "description": "Split markdown into chunks, e.g. {\"type\": \"recursive\", \"size\": 400}."
        }),
    );
    json!({ "type": "object", "properties": Value::Object(p), "additionalProperties": false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::validate;

    fn full() -> Capabilities {
        Capabilities::new(
            &["markdown", "html", "json"],
            &["wait", "click", "screenshot", "pdf"],
        )
        .with_interact(true)
    }

    /// 每个属性都是 `ScrapeOptions` 的真实字段；每个字段要么暴露，要么在 `NOT_EXPOSED` 里写明原因。
    #[test]
    fn every_property_is_a_real_field() {
        let fields = pagesift_api::fields::names::<pagesift_api::types::ScrapeOptions>();
        let schema = scrape_options_schema(&full());
        let props = schema["properties"].as_object().unwrap();
        for k in props.keys() {
            assert!(
                fields.contains(&k.as_str()),
                "{k} is not a ScrapeOptions field"
            );
        }
        for f in fields {
            let hidden = NOT_EXPOSED.iter().any(|(n, _)| n == f);
            assert!(
                props.contains_key(*f) != hidden,
                "{f}: exposed and hidden must differ"
            );
        }
    }

    #[test]
    fn formats_and_actions_come_from_capabilities() {
        let schema = scrape_options_schema(&full());
        let ok = json!({"formats": ["json", {"type": "markdown"}], "actions": [{"type": "pdf"}]});
        validate(&schema, &ok).unwrap();
        for bad in [
            json!({"formats": ["screenshot"]}),
            json!({"formats": [{"type": "branding"}]}),
            json!({"actions": [{"type": "generatePDF"}]}),
            json!({"zeroDataRetention": true}),
        ] {
            assert!(validate(&schema, &bad).is_err(), "{bad}");
        }
        let basic = scrape_options_schema(&Capabilities::basic());
        assert!(basic["properties"]["actions"].is_null());
        assert!(validate(&basic, &json!({"actions": []})).is_err());
    }
}
