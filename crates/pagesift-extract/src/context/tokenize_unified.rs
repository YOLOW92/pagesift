//! Unified lexical, code-symbol, entity-anchor, qualifier, and lightweight semantic sketch analyzer.
//!
//! Resolves the split between `bm25.rs` (exact lowercase words) and `relevance.rs` (stemmed terms)
//! while preserving exact API/code symbols, CJK bigrams/unigrams, and unit/negation guards.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::chunk::is_cjk;

// [推导] 轻量特征投影桶数 16（2^4），配合 L2 归一化构成零外部依赖语义近似内积草图
pub const SKETCH_DIM: usize = 16;
// [协议] FNV-1a 64 位标准初始偏移基数（IETF draft-eastlake-fnv）
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
// [协议] FNV-1a 64 位标准素数乘子（IETF draft-eastlake-fnv）
const FNV_PRIME: u64 = 0x0100_0000_01b3;
// [协议] 公历合理业务年份范围下界 1970（Unix 纪元起点）
pub(crate) const MIN_YEAR: u32 = 1970;
// [协议] 公历合理业务年份范围上界 2099
pub(crate) const MAX_YEAR: u32 = 2099;

/// Analyzed lexical, structural, and semantic-sketch features of a knowledge unit or query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LexicalFeatures {
    pub term_freqs: BTreeMap<String, u32>,
    pub exact_symbols: BTreeSet<String>,
    pub entity_anchors: Vec<String>,
    pub total_terms: u32,
    pub sketch: [f32; SKETCH_DIM],
    pub has_negation_or_qualifier: bool,
    pub has_units_or_currency: bool,
}

/// Canonical deduplicated query concept `(primary_term, optional_exact_lower)` so inflected or
/// camelCase query tokens never double-count the coverage denominator.
#[derive(Debug, Clone)]
pub(crate) struct QueryAnalysis {
    pub features: LexicalFeatures,
    pub lexical_concepts: Vec<(String, Option<String>)>,
}

/// Analyze a search/pack query into both [`LexicalFeatures`] and deduplicated query concepts.
pub(crate) fn analyze_query(query: &str) -> QueryAnalysis {
    let features = analyze_text(query);
    let mut seen_primary: BTreeSet<String> = BTreeSet::new();
    let mut lexical_concepts: Vec<(String, Option<String>)> = Vec::new();
    let mut latin_buf = String::new();
    let mut lower_buf = String::new();
    let mut stem_buf = String::new();
    let mut cjk_run: Vec<char> = Vec::new();

    let push_latin = |buf: &mut String,
                      l_buf: &mut String,
                      s_buf: &mut String,
                      seen: &mut BTreeSet<String>,
                      out: &mut Vec<(String, Option<String>)>| {
        if buf.is_empty() {
            return;
        }
        fill_ascii_lowercase(buf, l_buf);
        if !is_stopword(l_buf) {
            let stemmed = stem_ascii_into(l_buf, s_buf);
            if seen.insert(stemmed.to_owned()) {
                let alt = (stemmed != l_buf.as_str()).then(|| l_buf.clone());
                out.push((stemmed.to_owned(), alt));
            }
        }
        buf.clear();
    };

    let push_cjk = |run: &mut Vec<char>,
                    seen: &mut BTreeSet<String>,
                    out: &mut Vec<(String, Option<String>)>| {
        if run.is_empty() {
            return;
        }
        if run.len() == 1 {
            let mut utf8_buf = [0u8; 4]; // [协议] UTF-8 单字符最长 4 字节 (RFC 3629)
            let uni = run[0].encode_utf8(&mut utf8_buf);
            if seen.insert(uni.to_owned()) {
                out.push((uni.to_owned(), None));
            }
        } else {
            for w in run.windows(2) {
                let mut utf8_buf = [0u8; 8]; // [协议] UTF-8 双字符最长 8 字节 (RFC 3629)
                let n0 = w[0].encode_utf8(&mut utf8_buf).len();
                let n1 = w[1].encode_utf8(&mut utf8_buf[n0..]).len();
                // Safety: both characters were just encoded as valid UTF-8 into `utf8_buf[..n0+n1]`.
                let bi = std::str::from_utf8(&utf8_buf[..n0 + n1]).unwrap_or_default();
                if seen.insert(bi.to_owned()) {
                    out.push((bi.to_owned(), None));
                }
            }
        }
        run.clear();
    };

    for ch in query.chars() {
        if is_cjk(ch) {
            push_latin(
                &mut latin_buf,
                &mut lower_buf,
                &mut stem_buf,
                &mut seen_primary,
                &mut lexical_concepts,
            );
            if !is_cjk_punctuation(ch) {
                cjk_run.push(ch);
            } else {
                push_cjk(&mut cjk_run, &mut seen_primary, &mut lexical_concepts);
            }
        } else if ch.is_alphanumeric() {
            push_cjk(&mut cjk_run, &mut seen_primary, &mut lexical_concepts);
            latin_buf.push(ch);
        } else {
            push_latin(
                &mut latin_buf,
                &mut lower_buf,
                &mut stem_buf,
                &mut seen_primary,
                &mut lexical_concepts,
            );
            push_cjk(&mut cjk_run, &mut seen_primary, &mut lexical_concepts);
        }
    }
    push_latin(
        &mut latin_buf,
        &mut lower_buf,
        &mut stem_buf,
        &mut seen_primary,
        &mut lexical_concepts,
    );
    push_cjk(&mut cjk_run, &mut seen_primary, &mut lexical_concepts);

    QueryAnalysis {
        features,
        lexical_concepts,
    }
}

/// Analyze text into unified lexical terms, preserved code/API identifiers, anchors, and sketch.
pub fn analyze_text(text: &str) -> LexicalFeatures {
    let mut term_freqs: BTreeMap<String, u32> = BTreeMap::new();
    let mut exact_symbols: BTreeSet<String> = BTreeSet::new();
    let mut anchors_set: BTreeSet<String> = BTreeSet::new();
    let mut total_terms = 0u32;
    let mut raw_sketch = [0.0f32; SKETCH_DIM];
    let mut has_negation = false;
    let mut has_units = false;

    let mut lower_buf = String::with_capacity(32); // [策略] 复用单词缓冲，覆盖 99% 英文词与代码标识符免重复分配
    let mut stem_buf = String::with_capacity(32); // [策略] 复用词干缓冲避免每次调用堆分配

    // 1. Scan contiguous non-whitespace tokens to preserve code/API symbols, contractions, and anchors.
    for raw_tok in text.split_whitespace() {
        let trimmed = raw_tok.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '"' | '\'' | '`' | '|' | '>'
            )
        });
        if trimmed.is_empty() {
            continue;
        }
        if is_contracted_negation(trimmed) {
            has_negation = true;
        }
        if is_code_or_identifier_symbol(trimmed) {
            fill_ascii_lowercase(trimmed, &mut lower_buf);
            // [策略] 精确符号在特征草图中赋予 1.5 倍显著性权重
            add_sketch_feature(&mut raw_sketch, &lower_buf, 1.5);
            if !exact_symbols.contains(lower_buf.as_str()) {
                exact_symbols.insert(lower_buf.clone());
            }
        }
        collect_token_anchors(trimmed, &mut anchors_set);
    }

    // 2. Single-pass character scan for currency/unit glyphs, stemmed BM25 terms, and CJK bigrams.
    let mut latin_buf = String::with_capacity(32); // [策略] 复用字母数字扫描缓冲
    let mut cjk_run: Vec<char> = Vec::with_capacity(16); // [策略] 复用 CJK 连续短语缓冲

    let flush_latin = |buf: &mut String,
                       l_buf: &mut String,
                       s_buf: &mut String,
                       tf: &mut BTreeMap<String, u32>,
                       anchors: &mut BTreeSet<String>,
                       total: &mut u32,
                       sketch: &mut [f32; SKETCH_DIM],
                       neg: &mut bool,
                       unit: &mut bool| {
        if buf.is_empty() {
            return;
        }
        // [协议] 公历四位年份数字长度 4
        if buf.len() == 4
            && let Ok(yr) = buf.parse::<u32>()
            && (MIN_YEAR..=MAX_YEAR).contains(&yr)
        {
            let mut y_tag = String::with_capacity(9); // [协议] "year:YYYY" 固定 9 字节
            y_tag.push_str("year:");
            y_tag.push_str(buf);
            anchors.insert(y_tag);
        }
        fill_ascii_lowercase(buf, l_buf);
        if is_english_negation_or_qualifier(l_buf) {
            *neg = true;
        }
        if is_unit_or_currency_word(l_buf) {
            *unit = true;
        }
        if !is_stopword(l_buf) {
            let stemmed = stem_ascii_into(l_buf, s_buf);
            increment_term(tf, stemmed);
            *total = total.saturating_add(1);
            add_sketch_feature(sketch, stemmed, 1.0);
            if stemmed != l_buf.as_str() {
                increment_term(tf, l_buf);
            }
            add_char_ngrams(sketch, l_buf);
        }
        buf.clear();
    };

    let flush_cjk = |run: &mut Vec<char>,
                     tf: &mut BTreeMap<String, u32>,
                     total: &mut u32,
                     sketch: &mut [f32; SKETCH_DIM],
                     neg: &mut bool,
                     unit: &mut bool| {
        if run.is_empty() {
            return;
        }
        if contains_cjk_qualifier_chars(run) {
            *neg = true;
        }
        if contains_cjk_unit_chars(run) {
            *unit = true;
        }
        if run.len() == 1 {
            let mut utf8_buf = [0u8; 4]; // [协议] UTF-8 单字符最长 4 字节 (RFC 3629)
            let uni = run[0].encode_utf8(&mut utf8_buf);
            increment_term(tf, uni);
            *total = total.saturating_add(1);
            add_sketch_feature(sketch, uni, 1.0);
        } else {
            for w in run.windows(2) {
                let mut utf8_buf = [0u8; 8]; // [协议] UTF-8 双字符最长 8 字节 (RFC 3629)
                let n0 = w[0].encode_utf8(&mut utf8_buf).len();
                let n1 = w[1].encode_utf8(&mut utf8_buf[n0..]).len();
                let bi = std::str::from_utf8(&utf8_buf[..n0 + n1]).unwrap_or_default();
                increment_term(tf, bi);
                *total = total.saturating_add(1);
                add_sketch_feature(sketch, bi, 1.0);
            }
        }
        run.clear();
    };

    for ch in text.chars() {
        if matches!(
            ch,
            '$' | '€' | '£' | '¥' | '₩' | '₹' | '₽' | '₿' | '%' | '‰' | '℃'
        ) {
            has_units = true;
        }
        if is_cjk(ch) {
            flush_latin(
                &mut latin_buf,
                &mut lower_buf,
                &mut stem_buf,
                &mut term_freqs,
                &mut anchors_set,
                &mut total_terms,
                &mut raw_sketch,
                &mut has_negation,
                &mut has_units,
            );
            if !is_cjk_punctuation(ch) {
                cjk_run.push(ch);
            } else {
                flush_cjk(
                    &mut cjk_run,
                    &mut term_freqs,
                    &mut total_terms,
                    &mut raw_sketch,
                    &mut has_negation,
                    &mut has_units,
                );
            }
        } else if ch.is_alphanumeric() {
            flush_cjk(
                &mut cjk_run,
                &mut term_freqs,
                &mut total_terms,
                &mut raw_sketch,
                &mut has_negation,
                &mut has_units,
            );
            latin_buf.push(ch);
        } else {
            flush_latin(
                &mut latin_buf,
                &mut lower_buf,
                &mut stem_buf,
                &mut term_freqs,
                &mut anchors_set,
                &mut total_terms,
                &mut raw_sketch,
                &mut has_negation,
                &mut has_units,
            );
            flush_cjk(
                &mut cjk_run,
                &mut term_freqs,
                &mut total_terms,
                &mut raw_sketch,
                &mut has_negation,
                &mut has_units,
            );
        }
    }
    flush_latin(
        &mut latin_buf,
        &mut lower_buf,
        &mut stem_buf,
        &mut term_freqs,
        &mut anchors_set,
        &mut total_terms,
        &mut raw_sketch,
        &mut has_negation,
        &mut has_units,
    );
    flush_cjk(
        &mut cjk_run,
        &mut term_freqs,
        &mut total_terms,
        &mut raw_sketch,
        &mut has_negation,
        &mut has_units,
    );

    normalize_l2(&mut raw_sketch);

    LexicalFeatures {
        term_freqs,
        exact_symbols,
        entity_anchors: anchors_set.into_iter().collect(),
        total_terms,
        sketch: raw_sketch,
        has_negation_or_qualifier: has_negation,
        has_units_or_currency: has_units,
    }
}

#[inline]
fn increment_term(tf: &mut BTreeMap<String, u32>, key: &str) {
    if let Some(cnt) = tf.get_mut(key) {
        *cnt = cnt.saturating_add(1);
    } else {
        tf.insert(key.to_owned(), 1);
    }
}

#[inline]
fn fill_ascii_lowercase(src: &str, dst: &mut String) {
    dst.clear();
    dst.reserve(src.len());
    dst.push_str(src);
    dst.make_ascii_lowercase();
}

/// Fast allocation-free scan of 4-digit calendar years inside a heading or short string.
pub(crate) fn scan_years_into(text: &str, out: &mut BTreeSet<u64>) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    // [协议] 公历四位年份数字长度 4
    while i + 4 <= bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // [协议] 公历四位年份数字长度 4
            if i - start == 4
                && let Ok(s) = std::str::from_utf8(&bytes[start..i])
                && let Ok(yr) = s.parse::<u32>()
                && (MIN_YEAR..=MAX_YEAR).contains(&yr)
            {
                out.insert(u64::from(yr));
            }
        } else {
            i += 1;
        }
    }
}

/// Cosine similarity between two L2-normalized semantic sketches (`[0.0, 1.0]`).
pub fn sketch_similarity(a: &[f32; SKETCH_DIM], b: &[f32; SKETCH_DIM]) -> f32 {
    let mut dot = 0.0f32;
    for i in 0..SKETCH_DIM {
        dot += a[i] * b[i];
    }
    if dot.is_finite() {
        dot.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Weighted Jaccard similarity over term frequency maps (`[0.0, 1.0]`).
pub fn lexical_jaccard(a: &BTreeMap<String, u32>, b: &BTreeMap<String, u32>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut inter = 0u64;
    let mut union = 0u64;
    let mut it_a = a.iter().peekable();
    let mut it_b = b.iter().peekable();
    while it_a.peek().is_some() || it_b.peek().is_some() {
        match (it_a.peek(), it_b.peek()) {
            (Some(&(ka, &va)), Some(&(kb, &vb))) if ka == kb => {
                inter += u64::from(va.min(vb));
                union += u64::from(va.max(vb));
                it_a.next();
                it_b.next();
            }
            (Some(&(ka, &va)), Some(&(kb, _))) if ka < kb => {
                union += u64::from(va);
                it_a.next();
            }
            (Some(&(_, &va)), None) => {
                union += u64::from(va);
                it_a.next();
            }
            (_, Some(&(_, &vb))) => {
                union += u64::from(vb);
                it_b.next();
            }
            (None, None) => break,
        }
    }
    if union == 0 {
        0.0
    } else {
        (inter as f32 / union as f32).clamp(0.0, 1.0)
    }
}

fn is_code_or_identifier_symbol(tok: &str) -> bool {
    // [策略] 至少 2 个字节且不超过 96 字节，包含下划线、连字符参数、点路径、双冒号或驼峰时保留为精确符号
    if tok.len() < 2 || tok.len() > 96 {
        return false;
    }
    let has_sym_punct = tok.contains('_')
        || tok.starts_with("--")
        || tok.contains("::")
        || (tok.contains('-') && tok.bytes().any(|b| b.is_ascii_digit()))
        || (tok.contains('.') && tok.bytes().any(|b| b.is_ascii_alphabetic()));
    // UTF-8 safe: advance past the first Unicode scalar's exact byte width (never `&tok[1..]`)!
    let first_len = tok.chars().next().map_or(0, char::len_utf8);
    let has_camel = tok.bytes().any(|b| b.is_ascii_lowercase())
        && tok.as_bytes()[first_len..]
            .iter()
            .any(u8::is_ascii_uppercase);
    has_sym_punct || has_camel
}

fn is_contracted_negation(tok: &str) -> bool {
    // [策略] 英文否定缩写词长不超过 16 字节（兼容 ASCII `'` 与排版引号 `\u{2019}`）
    if !(tok.contains('\'') || tok.contains('\u{2019}')) || tok.len() > 16 {
        return false;
    }
    let norm: String = tok
        .chars()
        .map(|c| {
            if c == '\u{2019}' {
                '\''
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    matches!(
        norm.as_str(),
        "don't"
            | "doesn't"
            | "didn't"
            | "isn't"
            | "aren't"
            | "wasn't"
            | "weren't"
            | "won't"
            | "can't"
            | "couldn't"
            | "shouldn't"
            | "wouldn't"
            | "mustn't"
            | "haven't"
            | "hasn't"
            | "hadn't"
    )
}

fn collect_token_anchors(tok: &str, out: &mut BTreeSet<String>) {
    let clean =
        tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.');
    if clean.is_empty() {
        return;
    }
    let date_head = clean.split(['-', '/']).next().unwrap_or_default();
    // [协议] 公历四位年份数字长度 4（独立年份或 ISO 日期前缀 `2025-01-01`）
    if date_head.len() == 4
        && clean
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'/' || b == b'T' || b == b'Z')
        && let Ok(yr) = date_head.parse::<u32>()
        && (MIN_YEAR..=MAX_YEAR).contains(&yr)
    {
        out.insert(format!("year:{yr}"));
        return;
    }

    let id_clean = clean.trim_matches('.');
    // [策略] 长度 2..=48 的版本号（如 `v1.5`, `v2.0`）或型号/SKU/标准号识别为判别实体锚点
    if !(2..=48).contains(&id_clean.len()) {
        return;
    }
    let bytes = id_clean.as_bytes();
    // Explicit semantic version (`v1`, `v1.5.6`)
    if (bytes[0] == b'v' || bytes[0] == b'V')
        && bytes[1].is_ascii_digit()
        && bytes[1..].iter().all(|b| b.is_ascii_digit() || *b == b'.')
    {
        out.insert(format!("id:{}", id_clean.to_ascii_lowercase()));
        return;
    }
    // Explicit prefixed entity code even without digits (`SKU-A` vs `SKU-B`, `PLAN-PRO` vs `PLAN-ENT`)
    if starts_with_any_ignore_ascii_case(
        id_clean,
        &["sku-", "sku_", "model-", "cve-", "iso-", "rfc-"],
    ) {
        out.insert(format!("id:{}", id_clean.to_ascii_lowercase()));
        return;
    }
    // [策略] 至少 3 个字符防双字符缩写误判实体标识（`SKU-101`, `tier-2`, `PRO100`）
    if id_clean.len() >= 3
        && bytes.iter().any(u8::is_ascii_alphabetic)
        && bytes.iter().any(u8::is_ascii_digit)
        && (id_clean.contains('-')
            || id_clean.contains('_')
            || bytes.iter().take(2).all(u8::is_ascii_uppercase)
            || bytes.last().is_some_and(u8::is_ascii_digit))
    {
        out.insert(format!("id:{}", id_clean.to_ascii_lowercase()));
    }
}

#[inline]
fn starts_with_any_ignore_ascii_case(s: &str, prefixes: &[&str]) -> bool {
    let sb = s.as_bytes();
    prefixes.iter().any(|p| {
        let pb = p.as_bytes();
        sb.len() >= pb.len() && sb[..pb.len()].eq_ignore_ascii_case(pb)
    })
}

pub fn light_english_stem(word: &str) -> String {
    let mut scratch = String::new();
    stem_ascii_into(word, &mut scratch).to_owned()
}

fn stem_ascii_into<'a>(word: &'a str, scratch: &'a mut String) -> &'a str {
    // [策略] 仅对长度 >= 4 的纯 ASCII 词做轻量后缀归一，避免短词过度截断
    if word.len() < 4 || !word.is_ascii() {
        return word;
    }
    for suffix in [
        "ingly", "edly", "ments", "ment", "ings", "ing", "ations", "ation", "ies", "es", "ed", "s",
    ] {
        if suffix == "s" && (word.ends_with("ss") || word.ends_with("us") || word.ends_with("is")) {
            continue;
        }
        if let Some(stem) = word.strip_suffix(suffix) {
            // [策略] 词干至少保留 3 个字符防退化
            if stem.len() >= 3 {
                if suffix == "ies" {
                    scratch.clear();
                    scratch.reserve(stem.len() + 1);
                    scratch.push_str(stem);
                    scratch.push('y');
                    return scratch.as_str();
                }
                return stem;
            }
        }
    }
    word
}

fn add_char_ngrams(sketch: &mut [f32; SKETCH_DIM], word: &str) {
    // [策略] 对长度 >= 4 的 ASCII 词投影 4-gram 提升拼写变体与派生词的草图重叠度
    const NGRAM_LEN: usize = 4;
    // [推导] 子词权重设为整词的 1/4 防止常见词缀淹没主词义
    const NGRAM_WEIGHT: f32 = 0.25;
    if word.len() < NGRAM_LEN || !word.is_ascii() {
        return;
    }
    for win in word.as_bytes().windows(NGRAM_LEN) {
        let h = fnv1a(win);
        let idx = (h as usize) & (SKETCH_DIM - 1);
        // [协议] 取高 32 位最低位作 Rademacher 符号哈希
        let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
        sketch[idx] += sign * NGRAM_WEIGHT;
    }
}

fn add_sketch_feature(sketch: &mut [f32; SKETCH_DIM], token: &str, weight: f32) {
    let h = fnv1a(token.as_bytes());
    let idx = (h as usize) & (SKETCH_DIM - 1);
    // [协议] Signed feature hashing (Weinberger et al. 2009)
    let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
    sketch[idx] += sign * weight;
}

fn normalize_l2(sketch: &mut [f32; SKETCH_DIM]) {
    let mut norm_sq = 0.0f32;
    for v in sketch.iter() {
        norm_sq += v * v;
    }
    // Preserve true signed Rademacher coordinates on the unit sphere so zero-overlap OOD texts
    // retain unbiased E[<phi(x), phi(y)>] = 0 (Weinberger et al. 2009) instead of positive quadrant bias!
    if norm_sq > 0.0 && norm_sq.is_finite() {
        let inv = 1.0 / norm_sq.sqrt();
        for v in sketch.iter_mut() {
            *v *= inv;
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "for"
            | "from"
            | "in"
            | "into"
            | "is"
            | "it"
            | "of"
            | "on"
            | "or"
            | "that"
            | "the"
            | "their"
            | "this"
            | "to"
            | "was"
            | "were"
            | "will"
            | "with"
    )
}

fn is_english_negation_or_qualifier(w: &str) -> bool {
    matches!(
        w,
        "not"
            | "no"
            | "never"
            | "none"
            | "neither"
            | "nor"
            | "cannot"
            | "except"
            | "excluding"
            | "excludes"
            | "exclude"
            | "unless"
            | "without"
            | "only"
            | "deprecated"
            | "discontinued"
            | "experimental"
            | "prohibited"
            | "forbidden"
            | "conditional"
            | "contingent"
            | "restricted"
            | "capped"
            | "limit"
            | "max"
            | "min"
            | "maximum"
            | "minimum"
    )
}

fn is_unit_or_currency_word(w: &str) -> bool {
    matches!(
        w,
        "usd"
            | "eur"
            | "gbp"
            | "jpy"
            | "cny"
            | "rmb"
            | "cad"
            | "aud"
            | "chf"
            | "dollars"
            | "dollar"
            | "euro"
            | "euros"
            | "yen"
            | "yuan"
            | "percent"
            | "pct"
            | "ms"
            | "sec"
            | "secs"
            | "seconds"
            | "minute"
            | "minutes"
            | "hour"
            | "hours"
            | "day"
            | "days"
            | "daily"
            | "week"
            | "weeks"
            | "weekly"
            | "month"
            | "monthly"
            | "quarterly"
            | "year"
            | "annual"
            | "annually"
            | "kb"
            | "mb"
            | "gb"
            | "tb"
            | "kib"
            | "mib"
            | "gib"
            | "bps"
            | "mbps"
            | "gbps"
            | "rps"
            | "qps"
            | "hz"
            | "khz"
            | "mhz"
            | "ghz"
            | "kw"
            | "mw"
            | "kwh"
            | "mwh"
            | "kg"
            | "mg"
            | "km"
            | "cm"
            | "mm"
            | "token"
            | "tokens"
            | "seat"
            | "seats"
    )
}

fn contains_cjk_qualifier_chars(run: &[char]) -> bool {
    for &c in run {
        if matches!(
            c,
            '不' | '未'
                | '非'
                | '无'
                | '除'
                | '外'
                | '仅'
                | '限'
                | '禁'
                | '否'
                | '暫'
                | '暂'
                | '勿'
        ) {
            return true;
        }
    }
    // Check Japanese kana negations/exclusions (`ない`, `除く`, `限り`, `未満`)
    for w in run.windows(2) {
        if matches!(
            (w[0], w[1]),
            ('な', 'い') | ('除', 'く') | ('限', 'り') | ('未', '満')
        ) {
            return true;
        }
    }
    false
}

fn contains_cjk_unit_chars(run: &[char]) -> bool {
    run.iter().any(|&c| {
        matches!(
            c,
            '元' | '円'
                | '万'
                | '亿'
                | '秒'
                | '分'
                | '时'
                | '日'
                | '天'
                | '周'
                | '月'
                | '年'
                | '席'
        )
    })
}

fn is_cjk_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '。' | '，'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '（'
            | '）'
            | '【'
            | '】'
            | '《'
            | '》'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '—'
            | '…'
            | '　'
    )
}
