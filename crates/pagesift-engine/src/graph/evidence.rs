use pagesift_api::types::{DocChunk, Document};
use pagesift_extract::evidence::extract_structured_with_evidence;
use pagesift_graph::evidence::align_traces;
use pagesift_graph::{
    ClaimRecord, DocNode, DocumentInput, EvidenceSpan, ExtractionMethod, GraphOptions,
    PageGraphDelta, ProvenanceEdge, RawFieldTrace, SourceKind, StreamingGraphBuilder,
    SubjectKeySource, build_indexed_blocks, collect_json_traces, normalize_field_key,
    resolve_subject_key,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct CapturedMember {
    pub path: String,
    pub title: Option<String>,
    pub markdown: String,
    pub format_label: String,
    pub is_email_part: bool,
    pub raw_json: Option<String>,
    pub raw_html: Option<String>,
}

/// Extract evidence traces from HTML metadata islands (JSON-LD, Microdata, OpenGraph).
pub fn collect_structured_html_traces(html: &str, url: Option<&str>) -> Vec<RawFieldTrace> {
    if html.is_empty() {
        return Vec::new();
    }
    extract_structured_with_evidence(html, url).traces
}

/// Parse raw JSON text into recursive field traces.
pub fn collect_json_document_traces(
    raw_json: &str,
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    let Ok(val) = serde_json::from_str::<Value>(raw_json) else {
        return Vec::new();
    };
    let mut traces = Vec::new();
    let method = ExtractionMethod::RegexPattern {
        pattern: "json".to_string(),
        label: Some("json".to_string()),
    };
    collect_json_traces(&val, "", &method, true, source_url, &mut traces);
    traces
}

fn split_pipe_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.contains('|') {
        return None;
    }
    let inner = trimmed
        .strip_prefix('|')?
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap());
    let cells: Vec<String> = inner.split('|').map(|s| s.trim().to_string()).collect();
    if cells.is_empty() { None } else { Some(cells) }
}

fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

fn strip_md_emphasis(s: &str) -> &str {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix("**").and_then(|x| x.strip_suffix("**")) {
        return inner.trim();
    }
    if let Some(inner) = t.strip_prefix('*').and_then(|x| x.strip_suffix('*')) {
        return inner.trim();
    }
    t
}

fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let mut t = line.trim();
    if t.is_empty()
        || t.starts_with('#')
        || t.starts_with('|')
        || t.starts_with('>')
        || t.starts_with("```")
    {
        return None;
    }
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        t = rest.trim();
    }
    let (raw_k, raw_v) = t.split_once(':')?;
    let k = strip_md_emphasis(raw_k);
    let v = strip_md_emphasis(raw_v);
    if k.is_empty()
        || v.is_empty()
        || k.len() > 40 // [策略] key/value 键值对行识别长度与词数上限
        || v.len() > 160 // [策略] 键值对值长度上限
        || k.split_whitespace().count() > 4 // [策略] 键最大词数
        || v.split_whitespace().count() > 12 // [策略] 值最大词数
        || k.contains("://")
        || k.eq_ignore_ascii_case("http")
        || k.eq_ignore_ascii_case("https")
        || k.eq_ignore_ascii_case("mailto")
        || v.starts_with("//")
        || v.contains("](")
    {
        return None;
    }
    let first = k.chars().next()?;
    if !first.is_alphabetic() {
        return None;
    }
    Some((k.to_string(), v.to_string()))
}

/// Extract field traces from markdown pipe tables, fenced JSON blocks, and concise Key: Value lines.
pub fn collect_tabular_and_kv_traces(
    markdown: &str,
    source_url: Option<&str>,
) -> Vec<RawFieldTrace> {
    let mut traces = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;
    let mut in_code_fence = false;
    let mut fence_is_json = false;
    let mut json_buf = String::new();

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if let Some(after) = trimmed.strip_prefix("```") {
            if !in_code_fence {
                in_code_fence = true;
                fence_is_json = after.trim().eq_ignore_ascii_case("json");
                json_buf.clear();
            } else {
                if fence_is_json && !json_buf.trim().is_empty() {
                    traces.extend(collect_json_document_traces(&json_buf, source_url));
                }
                in_code_fence = false;
                fence_is_json = false;
                json_buf.clear();
            }
            i += 1;
            continue;
        }
        if in_code_fence {
            if fence_is_json {
                json_buf.push_str(line);
                json_buf.push('\n');
            }
            i += 1;
            continue;
        }

        if i + 1 < lines.len()
            && let Some(headers) = split_pipe_row(line)
            && let Some(sep) = split_pipe_row(lines[i + 1])
            && is_separator_row(&sep)
        {
            i += 2;
            let mut row_idx = 0usize;
            while i < lines.len() {
                let Some(cells) = split_pipe_row(lines[i]) else {
                    break;
                };
                for (col_idx, cell) in cells.iter().enumerate() {
                    let Some(header) = headers.get(col_idx) else {
                        continue;
                    };
                    if header.is_empty() || cell.is_empty() {
                        continue;
                    }
                    let norm_col = normalize_field_key(header);
                    if norm_col.is_empty() {
                        continue;
                    }
                    traces.push(RawFieldTrace {
                        field_path: format!("table.{row_idx}.{norm_col}"),
                        field_name: header.clone(),
                        value: Value::String(cell.clone()),
                        raw_quote: cell.clone(),
                        attr_name: None,
                        method: ExtractionMethod::RegexPattern {
                            pattern: "markdown_table".to_string(),
                            label: Some(header.clone()),
                        },
                        source_url: source_url.map(str::to_string),
                        visible_in_dom: true,
                    });
                }
                row_idx += 1;
                i += 1;
            }
            continue;
        }

        if let Some((k, v)) = parse_kv_line(line) {
            let norm_k = normalize_field_key(&k);
            if !norm_k.is_empty() {
                traces.push(RawFieldTrace {
                    field_path: format!("kv.{norm_k}"),
                    field_name: k.clone(),
                    value: Value::String(v),
                    raw_quote: trimmed.to_string(),
                    attr_name: None,
                    method: ExtractionMethod::RegexPattern {
                        pattern: "kv_line".to_string(),
                        label: Some(k),
                    },
                    source_url: source_url.map(str::to_string),
                    visible_in_dom: true,
                });
            }
        }
        i += 1;
    }

    traces
}

pub fn chunk_spans_of(chunks: Option<&[DocChunk]>) -> Vec<(usize, usize, usize)> {
    chunks
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(idx, c)| (idx, c.start, c.end))
        .collect()
}

/// Build an independent `PageGraphDelta` for an extracted archive member or email part.
pub fn build_member_delta(
    parent_url: &str,
    parent_doc_id: &str,
    opts: &GraphOptions,
    observed_at_ms: u64,
    member: &CapturedMember,
) -> PageGraphDelta {
    let member_url = format!("{parent_url}#{}", member.path);
    let source_kind = if member.is_email_part {
        SourceKind::EmailPart {
            mail_url: parent_url.to_string(),
            part_name: member.path.clone(),
        }
    } else {
        SourceKind::ArchiveMember {
            archive_url: parent_url.to_string(),
            member_path: member.path.clone(),
        }
    };

    let mut traces = Vec::new();
    if let Some(raw_json) = &member.raw_json {
        traces.extend(collect_json_document_traces(raw_json, Some(&member_url)));
    }
    if let Some(raw_html) = &member.raw_html {
        traces.extend(collect_structured_html_traces(raw_html, Some(&member_url)));
    }
    if traces.is_empty() {
        traces.extend(collect_tabular_and_kv_traces(
            &member.markdown,
            Some(&member_url),
        ));
    }

    let markdown = if member.markdown.is_empty() {
        member.raw_json.clone().unwrap_or_default()
    } else {
        member.markdown.clone()
    };

    let input = DocumentInput {
        requested_url: member_url.clone(),
        final_url: member_url.clone(),
        canonical_url: None,
        canonical_from_header: false,
        source_kind,
        observed_at_ms,
        status_code: 200, // [协议] HTTP 200 状态码代表归档/附件子成员成功解包
        engine: format!("office:{}", member.format_label),
        content_hash: None,
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: member.title.clone().or_else(|| Some(member.path.clone())),
        markdown,
        redirect_chain: Vec::new(),
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: Vec::new(),
        structured_json_ld: Vec::new(),
        extracted_fields: None,
        raw_traces: traces,
        chunk_spans: Vec::new(),
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    };

    let mut local_opts = opts.clone();
    local_opts.versions = false;
    let mut builder = StreamingGraphBuilder::new(local_opts);
    let mut delta = builder.ingest(input);
    delta.provenance.push(ProvenanceEdge::AttachmentOf {
        attachment_url: member_url,
        parent_url: parent_url.to_string(),
        parent_doc_id: Some(parent_doc_id.to_string()),
        media_id: None,
        member_path: Some(member.path.clone()),
    });
    delta.provenance.push(ProvenanceEdge::DerivedFrom {
        doc_id: delta.node.doc_id.clone(),
        source_url: parent_url.to_string(),
        parent_doc_id: Some(parent_doc_id.to_string()),
        relation: if member.is_email_part {
            "email_part".to_string()
        } else {
            "archive_member".to_string()
        },
    });
    delta
}

/// Align adaptive CSS/XPath/Regex extract traces onto the document's `GraphSlice`.
pub fn enrich_doc_with_extract_traces(
    doc: &mut Document,
    traces: &[RawFieldTrace],
    markdown: Option<&str>,
    opts: &GraphOptions,
) {
    if traces.is_empty() && doc.extract.is_none() {
        return;
    }
    let Some(slice) = doc.graph.as_mut() else {
        return;
    };
    let url = doc
        .metadata
        .url
        .clone()
        .or_else(|| doc.metadata.source_url.clone())
        .unwrap_or_default();
    let md = markdown.or(doc.markdown.as_deref()).unwrap_or_default();
    let spans = chunk_spans_of(doc.chunks.as_deref());
    let blocks = build_indexed_blocks(
        &slice.doc_id,
        md,
        &spans,
        opts.limits.max_blocks_per_doc,
        opts.limits.max_excerpt_chars,
    );

    if matches!(
        slice.subject_key.source,
        SubjectKeySource::CanonicalCluster | SubjectKeySource::ResourceKey
    ) {
        let resolved = resolve_subject_key(
            &opts.subject_keys,
            &[],
            traces,
            doc.metadata.canonical_url.as_deref(),
            &slice.resource_key,
        );
        let rank = |s: SubjectKeySource| match s {
            SubjectKeySource::ExplicitConfig => 0u8,
            SubjectKeySource::JsonLdId => 1,
            SubjectKeySource::ProductCode => 2,
            SubjectKeySource::CanonicalCluster => 3, // [推导] 实体主键优先级序号
            SubjectKeySource::ResourceKey => 4,      // [推导] 实体主键兜底序号
        };
        if rank(resolved.source) < rank(slice.subject_key.source) {
            slice.subject_key = resolved;
            for claim in &mut slice.claims {
                claim.subject_key = slice.subject_key.key.clone();
            }
        }
    }

    let aligned: Vec<EvidenceSpan> = if opts.evidence || opts.facts || opts.conflicts {
        align_traces(&slice.doc_id, &url, traces, &blocks)
    } else {
        Vec::new()
    };

    if opts.evidence {
        slice.evidence.extend(aligned.iter().cloned());
    }

    if (opts.facts || opts.conflicts) && !traces.is_empty() {
        let engine_label = doc
            .metadata
            .pagesift
            .as_ref()
            .and_then(|p| p.engine.clone())
            .unwrap_or_else(|| "extract".to_string());
        for (trace, ev) in traces.iter().zip(aligned.iter()) {
            let claim = ClaimRecord::from_trace(
                &slice.subject_key.key,
                &slice.resource_key,
                &slice.doc_id,
                &url,
                slice.source_kind.clone(),
                &engine_label,
                slice.observed_at_ms,
                doc.metadata.canonical_url.clone(),
                trace,
                ev.clone(),
            );
            slice.claims.push(claim);
        }
    }
    let _ = DocNode::logical_url;
}
