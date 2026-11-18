//! Unified structure-preserving [`KnowledgeUnit`] scanner bridging Markdown AST, `yolowhtml 0.1.2`
//! table `<caption>` / `<figcaption>` conventions, PDF page markers, and `DataGraph` claims/evidence.

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::context::{
    KnowledgeUnit, TableStructureMeta, UnitKind, UnitLineage, UnitLocation, UnitStatus,
};
use pagesift_graph::{
    BlockRef, ClaimRecord, DocNode, EvidenceSpan, SubjectKey, SubjectKeySource, TemporalSemantics,
};

use crate::chunk::markdown_ast::{BlockKind as AstKind, Doc as AstDoc, fence_marker};
use crate::chunk::{TokenIndex, token_spans};
use crate::context::tokenize_unified::analyze_text;

// [策略] 超长表格按数据行分片时的默认单分片软上限（512 tokens），确保每个分片保留完整表头可装入预算
const DEFAULT_MAX_TABLE_UNIT_TOKENS: usize = 512;

/// Fast allocation-free token counter matching `TokenIndex::new(s).len()` without building a `Vec`.
#[inline]
pub(crate) fn fast_token_count(text: &str) -> usize {
    token_spans(text).count()
}

/// Input bundle used to extract structured [`KnowledgeUnit`]s from one observed document.
pub struct UnitBuildInput<'a> {
    pub doc_node: &'a DocNode,
    pub markdown: &'a str,
    pub subject_key: Option<&'a SubjectKey>,
    pub temporal: Option<&'a TemporalSemantics>,
    pub blocks: &'a [BlockRef],
    pub claims: &'a [ClaimRecord],
    pub evidence: &'a [EvidenceSpan],
    pub origin_root: &'a str,
    pub origin_independent: bool,
    pub max_table_unit_tokens: usize,
}

impl<'a> UnitBuildInput<'a> {
    pub fn simple(doc_node: &'a DocNode, markdown: &'a str) -> Self {
        Self {
            doc_node,
            markdown,
            subject_key: None,
            temporal: None,
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root: &doc_node.resource_key,
            origin_independent: true,
            max_table_unit_tokens: DEFAULT_MAX_TABLE_UNIT_TOKENS,
        }
    }
}

/// Extract structured, provenance-grounded [`KnowledgeUnit`]s from a document observation.
pub fn extract_units_from_document(input: &UnitBuildInput<'_>) -> Vec<KnowledgeUnit> {
    let md = input.markdown;
    if md.trim().is_empty() {
        return Vec::new();
    }
    let ast = AstDoc::scan(md, true);
    let tok_idx = TokenIndex::new(md);
    let page_markers = scan_page_markers(md);
    // [策略] 表格分片最小下限 64 tokens 容纳表头+至少一行
    let max_tbl_tokens = input.max_table_unit_tokens.max(64);

    type RawScannedItem = (
        UnitKind,
        usize,
        usize,
        String,
        Option<String>,
        Option<TableStructureMeta>,
    );
    let mut raw_items: Vec<RawScannedItem> = Vec::with_capacity(ast.blocks.len());

    let mut idx = 0;
    while idx < ast.blocks.len() {
        let blk = &ast.blocks[idx];
        let slice = &md[blk.range.clone()];
        let trimmed = slice.trim();
        if trimmed.is_empty() {
            idx += 1;
            continue;
        }
        let trim_lead = slice.len() - slice.trim_start().len();
        let byte_start = blk.range.start + trim_lead;
        let byte_end = byte_start + trimmed.len();

        // Pair leading `Table N:` / caption paragraph emitted by `yolowhtml 0.1.2` with an immediately following Table!
        if blk.kind == AstKind::Text
            && idx + 1 < ast.blocks.len()
            && ast.blocks[idx + 1].kind == AstKind::Table
            && looks_like_table_caption(trimmed)
        {
            let tbl_blk = &ast.blocks[idx + 1];
            let tbl_raw = &md[tbl_blk.range.clone()];
            let tbl_lead = tbl_raw.len() - tbl_raw.trim_start().len();
            let tbl_slice = tbl_raw.trim();
            let tbl_body_start = tbl_blk.range.start + tbl_lead;
            let tbl_end = tbl_body_start + tbl_slice.len();
            emit_table_units(
                tbl_slice,
                tbl_body_start,
                md,
                Some(trimmed.to_owned()),
                byte_start..tbl_end,
                &tok_idx,
                max_tbl_tokens,
                &mut raw_items,
            );
            idx += 2;
            continue;
        }

        match blk.kind {
            AstKind::Heading => {
                raw_items.push((
                    UnitKind::SectionHeading,
                    byte_start,
                    byte_end,
                    trimmed.to_owned(),
                    None,
                    None,
                ));
            }
            AstKind::Code => {
                let lang = parse_code_fence_lang(trimmed);
                raw_items.push((
                    UnitKind::CodeBlock,
                    byte_start,
                    byte_end,
                    trimmed.to_owned(),
                    lang,
                    None,
                ));
            }
            AstKind::Table => {
                emit_table_units(
                    trimmed,
                    byte_start,
                    md,
                    None,
                    byte_start..byte_end,
                    &tok_idx,
                    max_tbl_tokens,
                    &mut raw_items,
                );
            }
            AstKind::Text => {
                let kind = classify_text_block(trimmed);
                raw_items.push((kind, byte_start, byte_end, trimmed.to_owned(), None, None));
            }
        }
        idx += 1;
    }

    let mut occurrence_tracker: BTreeMap<(UnitKind, String, String), usize> = BTreeMap::new();
    let mut units = Vec::with_capacity(raw_items.len());

    for (kind, byte_start, byte_end, text, code_language, table_meta) in raw_items {
        let heading_path = ast.path_at(byte_start);
        let page_number = lookup_page(&page_markers, byte_start);
        let norm = pagesift_diff::normalize(&text, kind == UnitKind::CodeBlock);
        let content_hash = hash_unit_content(kind, &norm, table_meta.as_ref());
        let lex = analyze_text(&text);
        let tokens = if table_meta.as_ref().is_some_and(|m| m.is_slice) {
            fast_token_count(&text).max(1)
        } else {
            tok_idx.count_range(md, byte_start..byte_end).max(1)
        };

        let overlapping_blocks: Vec<String> = input
            .blocks
            .iter()
            .filter(|b| b.start < byte_end && b.end > byte_start)
            .map(|b| b.block_id.clone())
            .collect();

        let mut unit_spans: Vec<EvidenceSpan> = input
            .evidence
            .iter()
            .filter(|ev| span_overlaps(ev, byte_start, byte_end, &overlapping_blocks))
            .cloned()
            .collect();

        let mut claim_ids = Vec::new();
        let mut claim_temporal: Option<TemporalSemantics> = None;
        let mut unit_subject: Option<SubjectKey> = input.subject_key.cloned();
        for clm in input.claims {
            if span_overlaps(&clm.evidence, byte_start, byte_end, &overlapping_blocks) {
                claim_ids.push(clm.claim_id.clone());
                if unit_spans.iter().all(|s| s.field_path != clm.field_path) {
                    unit_spans.push(clm.evidence.clone());
                }
                if claim_temporal.is_none() && clm.temporal.is_some() {
                    claim_temporal = clm.temporal.clone();
                }
                if unit_subject.is_none() {
                    unit_subject = Some(SubjectKey {
                        key: clm.subject_key.clone(),
                        source: SubjectKeySource::ResourceKey,
                        raw: clm.subject_key.clone(),
                    });
                }
            }
        }
        claim_ids.sort();
        claim_ids.dedup();

        let effective_temporal = claim_temporal.or_else(|| input.temporal.cloned());
        let structural_anchor = heading_path.join(" > ");
        let occ_key = (kind, structural_anchor.clone(), content_hash.clone());
        let occ = *occurrence_tracker
            .entry(occ_key)
            .and_modify(|c| *c += 1)
            .or_insert(0);

        let unit_id = KnowledgeUnit::mint_unit_id(
            &input.doc_node.resource_key,
            kind,
            &structural_anchor,
            &content_hash,
            occ,
        );
        let revision_id = KnowledgeUnit::compute_revision_id(
            &unit_id,
            &content_hash,
            &claim_ids,
            effective_temporal.as_ref(),
        );

        units.push(KnowledgeUnit {
            unit_id,
            revision_id,
            content_hash,
            kind,
            text,
            tokens,
            subject_key: unit_subject,
            entity_anchors: lex.entity_anchors,
            has_negation_or_qualifier: lex.has_negation_or_qualifier,
            has_units_or_currency: lex.has_units_or_currency,
            location: UnitLocation {
                doc_id: input.doc_node.doc_id.clone(),
                resource_key: input.doc_node.resource_key.clone(),
                source_url: input.doc_node.final_url.clone(),
                source_kind: input.doc_node.source_kind.clone(),
                block_ids: overlapping_blocks,
                byte_start,
                byte_end,
                page_number,
                heading_path,
                code_language,
                table_meta,
            },
            lineage: UnitLineage::created(),
            status: UnitStatus::Active,
            temporal: effective_temporal,
            origin_root: input.origin_root.to_owned(),
            origin_independent: input.origin_independent,
            claim_ids,
            evidence_spans: unit_spans,
            observed_at_ms: input.doc_node.observed_at_ms,
        });
    }

    units
}

type RawTableSliceItem = (
    UnitKind,
    usize,
    usize,
    String,
    Option<String>,
    Option<TableStructureMeta>,
);

#[allow(clippy::too_many_arguments)]
fn emit_table_units(
    table_md: &str,
    tbl_body_start: usize,
    full_md: &str,
    caption: Option<String>,
    byte_range: std::ops::Range<usize>,
    tok_idx: &TokenIndex,
    max_tokens: usize,
    out: &mut Vec<RawTableSliceItem>,
) {
    let byte_start = byte_range.start;
    let byte_end = byte_range.end;

    let mut line_spans: Vec<(&str, usize, usize)> = Vec::new();
    let mut rel_pos = 0usize;
    for raw_line in table_md.split_inclusive('\n') {
        let clean = raw_line.trim_end_matches(['\n', '\r']);
        let l_start = tbl_body_start + rel_pos;
        let l_end = l_start + clean.len();
        line_spans.push((clean, l_start, l_end));
        rel_pos += raw_line.len();
    }

    let headers = line_spans
        .first()
        .map(|&(l, _, _)| parse_gfm_cells(l))
        .unwrap_or_default();
    let col_count = headers.len();
    let has_delim = line_spans
        .get(1)
        .is_some_and(|&(l, _, _)| is_table_delimiter_row(l));
    let body_start = if has_delim { 2 } else { 1 };
    let body_rows = if body_start <= line_spans.len() {
        &line_spans[body_start..]
    } else {
        &[][..]
    };

    let whole_text = match &caption {
        Some(cap) => format!("{cap}\n\n{table_md}"),
        None => table_md.to_owned(),
    };
    // [推导] 附加 caption 时按每 token 约 4 字节做保守下界兜底
    let approx_tokens = tok_idx
        .count_range(full_md, byte_start..byte_end)
        .max(whole_text.len() / 4);

    if approx_tokens <= max_tokens || body_rows.len() <= 2 || !has_delim {
        let meta = TableStructureMeta {
            caption,
            headers,
            row_count: body_rows.len(),
            col_count,
            is_slice: false,
        };
        out.push((
            UnitKind::Table,
            byte_start,
            byte_end,
            whole_text,
            None,
            Some(meta),
        ));
        return;
    }

    // Large table: split into row slices while re-injecting caption + header + delimiter in every slice,
    // and track exact per-slice byte coordinates in `full_md` so claims/provenance bind accurately!
    let header_prefix = match &caption {
        Some(cap) => format!("{cap}\n\n{}\n{}", line_spans[0].0, line_spans[1].0),
        None => format!("{}\n{}", line_spans[0].0, line_spans[1].0),
    };
    let prefix_tokens = fast_token_count(&header_prefix);
    // [策略] 每分片至少预留 16 tokens 放数据行
    let row_budget = max_tokens.saturating_sub(prefix_tokens).max(16);

    let mut cur_rows: Vec<&str> = Vec::new();
    let mut cur_toks = 0usize;
    let mut slice_start = byte_start;
    let mut slice_end = byte_end;
    let mut first_slice = true;

    for &(row, r_start, r_end) in body_rows {
        let r_toks = fast_token_count(row).max(1);
        if !cur_rows.is_empty() && cur_toks + r_toks > row_budget {
            let rendered = format!("{header_prefix}\n{}", cur_rows.join("\n"));
            let meta = TableStructureMeta {
                caption: caption.clone(),
                headers: headers.clone(),
                row_count: cur_rows.len(),
                col_count,
                is_slice: true,
            };
            out.push((
                UnitKind::Table,
                slice_start,
                slice_end,
                rendered,
                None,
                Some(meta),
            ));
            cur_rows.clear();
            cur_toks = 0;
            first_slice = false;
        }
        if cur_rows.is_empty() {
            slice_start = if first_slice { byte_start } else { r_start };
        }
        slice_end = r_end;
        cur_rows.push(row);
        cur_toks += r_toks;
    }
    if !cur_rows.is_empty() {
        let rendered = format!("{header_prefix}\n{}", cur_rows.join("\n"));
        let meta = TableStructureMeta {
            caption,
            headers,
            row_count: cur_rows.len(),
            col_count,
            is_slice: true,
        };
        out.push((
            UnitKind::Table,
            slice_start,
            slice_end,
            rendered,
            None,
            Some(meta),
        ));
    }
}

fn hash_unit_content(
    kind: UnitKind,
    normalized_text: &str,
    table_meta: Option<&TableStructureMeta>,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(kind.as_str().as_bytes());
    h.update(&[0]);
    if let Some(tm) = table_meta {
        for hdr in &tm.headers {
            h.update(hdr.as_bytes());
            // [协议] ASCII Unit Separator 0x1F 分隔列名防拼接歧义
            h.update(&[0x1f]);
        }
    }
    h.update(normalized_text.as_bytes());
    let hex = h.finalize().to_hex();
    // [协议] 8 字节（16 十六进制字符）紧凑内容指纹前缀
    format!("uch_{}", &hex[..16])
}

fn looks_like_table_caption(trimmed: &str) -> bool {
    // [策略] 单行且长度 <= 240 字节的表题前缀（对齐 yolowhtml 0.1.2 `<caption>` 渲染形态）
    if trimmed.contains('\n') || trimmed.len() > 240 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("table ")
        || lower.starts_with("table:")
        || lower.starts_with("tab.")
        || trimmed.starts_with("表")
        || trimmed.starts_with("表 ")
}

fn classify_text_block(trimmed: &str) -> UnitKind {
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.starts_with("![](")
        || (trimmed.starts_with("![") && trimmed.contains("]("))
        || lower.starts_with("figure ")
        || lower.starts_with("fig.")
        || trimmed.starts_with("图 ")
        || trimmed.starts_with("图1")
        || trimmed.starts_with("図")
    {
        return UnitKind::FigureCaption;
    }
    if trimmed.lines().all(|l| l.trim_start().starts_with('>')) {
        return UnitKind::Quote;
    }
    let list_lines = trimmed
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("+ ")
                || t.bytes().take_while(|b| b.is_ascii_digit()).count() > 0
                    && t.trim_start_matches(|c: char| c.is_ascii_digit())
                        .starts_with(". ")
        })
        .count();
    if list_lines * 2 >= trimmed.lines().count().max(1) && list_lines > 0 {
        return UnitKind::ListGroup;
    }
    UnitKind::Paragraph
}

fn parse_code_fence_lang(trimmed: &str) -> Option<String> {
    let first = trimmed.lines().next()?.trim();
    let rest = first.trim_start_matches(['`', '~']).trim();
    let lang = rest.split_whitespace().next().unwrap_or_default();
    (!lang.is_empty()).then(|| lang.to_ascii_lowercase())
}

/// Split a GFM table row on unescaped `|` pipes, preserving empty cells and unescaping `\|`.
pub(crate) fn parse_gfm_cells(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if s.is_empty() {
        return Vec::new();
    }
    if let Some(rest) = s.strip_prefix('|') {
        s = rest;
    }
    if s.ends_with('|') && !ends_with_escaped_pipe(s) {
        s = &s[..s.len() - 1];
    }

    let mut cells = Vec::new();
    let mut cur = String::with_capacity(s.len().min(32)); // [策略] 单元格常用短缓冲容量 32
    let mut chars = s.chars().peekable();
    let mut saw_pipe = line.trim().contains('|');
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            cur.push('|');
        } else if ch == '|' {
            saw_pipe = true;
            cells.push(cur.trim().to_owned());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if saw_pipe || !cur.trim().is_empty() {
        cells.push(cur.trim().to_owned());
    }
    if cells.len() == 1 && cells[0].is_empty() {
        return Vec::new();
    }
    cells
}

fn ends_with_escaped_pipe(s: &str) -> bool {
    if !s.ends_with('|') {
        return false;
    }
    let prefix = &s[..s.len() - 1];
    let backslashes = prefix.bytes().rev().take_while(|&b| b == b'\\').count();
    backslashes % 2 == 1
}

/// Check whether `line` is a valid GFM table delimiter row (`| --- | :---: |`), aligned with `markdown_ast`.
pub(crate) fn is_table_delimiter_row(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('|') || !t.contains('-') {
        return false;
    }
    let cells = parse_gfm_cells(t);
    let mut non_empty = 0usize;
    for cell in &cells {
        let stripped: String = cell.chars().filter(|c| !c.is_whitespace()).collect();
        if stripped.is_empty() {
            continue;
        }
        let inner = stripped.trim_matches(':');
        if inner.is_empty() || !inner.bytes().all(|b| b == b'-') {
            return false;
        }
        non_empty += 1;
    }
    non_empty > 0
}

/// Extract primary-column (column 0) non-empty row keys from a GFM table unit for row-entity guarding.
pub(crate) fn extract_table_primary_keys(text: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut delim_seen = false;
    for line in text.lines() {
        let t = line.trim();
        if !t.contains('|') {
            continue;
        }
        if !delim_seen {
            if is_table_delimiter_row(t) {
                delim_seen = true;
            }
            continue;
        }
        let cells = parse_gfm_cells(t);
        if let Some(first) = cells.first() {
            let norm = first.trim().to_ascii_lowercase();
            // Ignore empty cells or pure numeric row counters (`1`, `2`, `#1`)
            if !norm.is_empty()
                && norm.as_bytes().iter().any(u8::is_ascii_alphabetic)
                && !matches!(norm.as_str(), "note" | "notes" | "total" | "summary")
            {
                keys.insert(norm);
            }
        }
    }
    keys
}

fn span_overlaps(
    ev: &EvidenceSpan,
    byte_start: usize,
    byte_end: usize,
    overlapping_blocks: &[String],
) -> bool {
    if let (Some(s), Some(e)) = (ev.start, ev.end)
        && s < byte_end
        && e > byte_start
    {
        return true;
    }
    ev.block_id
        .as_ref()
        .is_some_and(|bid| overlapping_blocks.iter().any(|b| b == bid))
}

fn scan_page_markers(md: &str) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut active_fence: Option<(u8, usize)> = None;
    for line in md.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed_spaces = body.trim_start_matches(' ');
        if let Some((f_ch, f_len)) = active_fence {
            if trimmed_spaces.as_bytes().first().copied() == Some(f_ch)
                && fence_marker(body).is_some_and(|(c, l)| {
                    c == f_ch
                        && l >= f_len
                        && trimmed_spaces
                            .trim_end()
                            .trim_start_matches(char::from(f_ch))
                            .is_empty()
                })
            {
                active_fence = None;
            }
            pos += line.len();
            continue;
        }
        if matches!(
            trimmed_spaces.as_bytes().first().copied(),
            Some(b'`' | b'~')
        ) && let Some(marker) = fence_marker(body)
        {
            active_fence = Some(marker);
            pos += line.len();
            continue;
        }

        let t = body.trim();
        // Recognizes `<!-- page 3 -->` or `<!-- page: 3 -->` emitted by PDF/OCR pipelines outside code blocks.
        if let Some(inner) = t.strip_prefix("<!--").and_then(|s| s.strip_suffix("-->")) {
            let low = inner.trim().to_ascii_lowercase();
            if let Some(num_str) = low
                .strip_prefix("page:")
                .or_else(|| low.strip_prefix("page "))
                && let Ok(p) = num_str.trim().parse::<u32>()
            {
                out.push((pos, p));
            }
        }
        pos += line.len();
    }
    out
}

fn lookup_page(markers: &[(usize, u32)], byte_offset: usize) -> Option<u32> {
    if markers.is_empty() {
        return None;
    }
    let idx = markers.partition_point(|&(off, _)| off <= byte_offset);
    idx.checked_sub(1).map(|i| markers[i].1)
}
