//! crw-extract（AGPL-3.0，只在评测里用）：HTML 快照 → Markdown。
//! 参数取 crw 服务端默认值：only_main_content = true（crw-core config.rs 的 ExtractionConfig::default）。
use crw_core::types::OutputFormat;
use crw_extract::ExtractOptions;

fn main() {
    mix_proto::serve("crw", "crw-extract 0.39.0", |input| {
        let html = mix_proto::read_lossy(input)?;
        let formats = [OutputFormat::Markdown];
        let doc = crw_extract::extract(ExtractOptions {
            raw_html: &html,
            content_type: None,
            source_url: "",
            status_code: 200,
            rendered_with: Some("http".into()),
            elapsed_ms: 0,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            formats: &formats,
            only_main_content: true,
            include_tags: &[],
            exclude_tags: &[],
            css_selector: None,
            xpath: None,
            chunk_strategy: None,
            query: None,
            filter_mode: None,
            top_k: None,
            domain_selectors: None,
            captured_responses: &[],
            llm_fallback: None,
            debug: false,
            debug_sink: None,
            normalize_tables: false,
        })
        .map_err(|e| e.to_string())?;
        Ok(doc.markdown.unwrap_or_default())
    });
}
