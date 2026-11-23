//! Context-preserving atomic claim decomposition (informed by FActScore & VeriScore / MontageLie
//! lessons): splits natural-language answers into verifiable claims while preserving qualifiers,
//! negations, quantifiers, temporal anchors, causal markers, and inline citations.

use std::collections::BTreeSet;

use pagesift_graph::resolve::{AtomicClaim, ClaimQuantifier};

use crate::context::tokenize_unified::{MAX_YEAR, MIN_YEAR, analyze_text, scan_years_into};

// [策略] 单次声明拆分最大原子声明上限 64，防止超长输入造成 O(N*M) 核验延迟尖峰
pub const MAX_DECOMPOSED_CLAIMS: usize = 64;

/// Decompose a natural-language answer into context-preserving [`AtomicClaim`] entries.
/// If explicit `caller_claims` are provided, analyzes each directly without re-splitting.
pub fn decompose_claims(answer: &str, caller_claims: &[String]) -> Vec<AtomicClaim> {
    if !caller_claims.is_empty() {
        return caller_claims
            .iter()
            .filter(|s| !s.trim().is_empty())
            .take(MAX_DECOMPOSED_CLAIMS)
            .enumerate()
            .map(|(idx, raw)| analyze_atomic_claim(idx, raw.trim()))
            .collect();
    }

    let raw_sentences = split_sentences_preserving_context(answer);
    let mut clauses = Vec::new();
    for sent in raw_sentences {
        for sub in split_independent_conjunctions(&sent) {
            let trimmed = sub.trim();
            if !trimmed.is_empty() {
                clauses.push(trimmed.to_owned());
            }
            if clauses.len() >= MAX_DECOMPOSED_CLAIMS {
                break;
            }
        }
        if clauses.len() >= MAX_DECOMPOSED_CLAIMS {
            break;
        }
    }

    clauses
        .into_iter()
        .enumerate()
        .map(|(idx, text)| analyze_atomic_claim(idx, &text))
        .collect()
}

/// Analyze a single atomic claim text into rich structural, logical, temporal, and citation features.
pub fn analyze_atomic_claim(index: usize, text: &str) -> AtomicClaim {
    let trimmed = text.trim();
    let lex = analyze_text(trimmed);
    let lower = trimmed.to_ascii_lowercase();

    let cited_refs = extract_inline_citations(trimmed);
    let qualifiers = extract_qualifier_clauses(trimmed, &lower);
    let negated = detect_claim_negation(&lower, trimmed);
    let quantifier = detect_quantifier(&lower, trimmed);
    let asserts_causality = detect_causal_assertion(&lower, trimmed);

    let mut years_set = BTreeSet::new();
    scan_years_into(trimmed, &mut years_set);
    for a in &lex.entity_anchors {
        if let Some(y) = a.strip_prefix("year:")
            && let Ok(yr) = y.parse::<u64>()
            && (u64::from(MIN_YEAR)..=u64::from(MAX_YEAR)).contains(&yr)
        {
            years_set.insert(yr);
        }
    }

    let mut anchors: BTreeSet<String> = lex.entity_anchors.into_iter().collect();
    for raw_sym in &lex.exact_symbols {
        let sym = raw_sym.trim_end_matches(['.', ',', ';', ':', '?', '!', ')', ']', '"', '\'']);
        let is_real_symbol = !sym.is_empty()
            && (sym.contains('_')
                || sym.contains('-')
                || sym.contains("::")
                || sym.contains('.')
                || sym.contains('/')
                || sym.chars().any(|c| c.is_ascii_digit()));
        if is_real_symbol {
            anchors.insert(format!("sym:{sym}"));
        }
    }
    extract_named_entity_anchors(trimmed, &mut anchors);

    let (verifiable, unverifiable_reason) =
        classify_verifiability(trimmed, &lower, &lex.term_freqs);

    AtomicClaim {
        claim_id: AtomicClaim::compute_id(index, trimmed),
        index,
        text: trimmed.to_owned(),
        qualifiers,
        entity_anchors: anchors.into_iter().collect(),
        asserted_years: years_set.into_iter().collect(),
        cited_refs,
        negated,
        quantifier,
        asserts_causality,
        verifiable,
        unverifiable_reason,
    }
}

/// Split text into sentences while protecting decimal numbers, version numbers (`v1.6.3`),
/// URLs, abbreviations (`e.g.`, `i.e.`, `vs.`), and parenthesized / bracketed spans.
fn split_sentences_preserving_context(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line
            .trim()
            .trim_start_matches(['-', '*', '•', '·'])
            .trim();
        if line.is_empty() {
            continue;
        }
        // Strip leading markdown numbered list prefix like `"1. "` or `"2) "`.
        let line = strip_list_number_prefix(line);
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0usize;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut in_quotes = false;

        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            match c {
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                '[' => bracket_depth += 1,
                ']' => bracket_depth = bracket_depth.saturating_sub(1),
                '"' => in_quotes = !in_quotes,
                _ => {}
            }
            if paren_depth == 0 && bracket_depth == 0 && !in_quotes {
                let is_cjk_end = matches!(c, '。' | '！' | '？' | '；');
                let is_ascii_semi = c == ';';
                let is_ascii_period =
                    matches!(c, '.' | '!' | '?') && !is_protected_period(&chars, i);
                if is_cjk_end || is_ascii_semi || is_ascii_period {
                    // Absorb trailing inline citation brackets immediately after period (e.g. `.$49 [ref-1]`)
                    let mut end = i + 1;
                    while end < chars.len() && chars[end] == ' ' {
                        end += 1;
                    }
                    if end < chars.len() && chars[end] == '[' {
                        let mut b = end;
                        while b < chars.len() && chars[b] != ']' {
                            b += 1;
                        }
                        if b < chars.len() && chars[b] == ']' {
                            end = b + 1;
                            i = b;
                        } else {
                            end = i + 1;
                        }
                    } else {
                        end = i + 1;
                    }
                    let seg: String = chars[start..end].iter().collect();
                    let t = seg.trim();
                    if !t.is_empty() {
                        out.push(t.to_owned());
                    }
                    start = end;
                }
            }
            i += 1;
        }
        if start < chars.len() {
            let seg: String = chars[start..].iter().collect();
            let t = seg.trim();
            if !t.is_empty() {
                out.push(t.to_owned());
            }
        }
    }
    out
}

fn strip_list_number_prefix(s: &str) -> &str {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < b.len() && matches!(b[i], b'.' | b')') && b[i + 1] == b' ' {
        s[i + 2..].trim()
    } else {
        s
    }
}

fn is_protected_period(chars: &[char], idx: usize) -> bool {
    if chars[idx] != '.' {
        return false;
    }
    let prev = idx.checked_sub(1).and_then(|k| chars.get(k)).copied();
    let next = chars.get(idx + 1).copied();
    // Decimal numbers / versions / domains / filenames (`49.99`, `v1.7`, `vendor.example`)
    if let (Some(p), Some(n)) = (prev, next) {
        if p.is_ascii_digit() && n.is_ascii_digit() {
            return true;
        }
        if p.is_ascii_alphanumeric() && n.is_ascii_alphanumeric() {
            return true;
        }
        if n == '/' || n == ':' {
            return true;
        }
    }
    // [策略] 回溯最多 4 个字符识别常见英文缩写（e.g. / i.e. / vs. / Inc. / No.）避免误断句
    let prefix: String = chars[idx.saturating_sub(4)..=idx].iter().collect();
    let low = prefix.to_ascii_lowercase();
    if low.ends_with("e.g.")
        || low.ends_with("i.e.")
        || low.ends_with("vs.")
        || low.ends_with("etc.")
        || low.ends_with("inc.")
        || low.ends_with("ltd.")
        || low.ends_with("dr.")
        || low.ends_with("mr.")
        || low.ends_with("no.")
    {
        return true;
    }
    false
}

/// Split compound sentences on coordinating conjunctions ONLY when both sides look like complete
/// independent clauses AND neither side is a conditional/exception qualifier (`unless`, `except`, `only if`).
fn split_independent_conjunctions(sentence: &str) -> Vec<String> {
    let low = sentence.to_ascii_lowercase();
    // Never break conditional, causal, or exception structures (prevents MontageLie / context loss!).
    for guard in [
        " unless ",
        " except ",
        " only if ",
        " provided that ",
        " because ",
        " leads to ",
        " causes ",
        " 除非",
        " 除了",
        " 仅在",
        " 导致",
        " 因为",
    ] {
        if low.contains(guard) || sentence.contains(guard) {
            return vec![sentence.to_owned()];
        }
    }
    // Split on `", and "` or `", while "` or `", whereas "` when both halves contain a verb/copula or number.
    for sep in [
        ", and ",
        ", while ",
        ", whereas ",
        ", but ",
        "，而",
        "，且",
        "，但",
    ] {
        if let Some(pos) = sentence.find(sep) {
            let left = sentence[..pos].trim();
            let right = sentence[pos + sep.len()..].trim();
            // [策略] 左右分句均至少 10 字节且都包含谓词或数字时才安全拆分
            if left.len() >= 10
                && right.len() >= 10
                && has_clause_predicate(left)
                && has_clause_predicate(right)
            {
                let mut res = split_independent_conjunctions(left);
                res.extend(split_independent_conjunctions(right));
                return res;
            }
        }
    }
    vec![sentence.to_owned()]
}

fn has_clause_predicate(clause: &str) -> bool {
    if clause.bytes().any(|b| b.is_ascii_digit()) {
        return true;
    }
    let low = clause.to_ascii_lowercase();
    for v in [
        " is ",
        " are ",
        " was ",
        " were ",
        " has ",
        " have ",
        " costs ",
        " cost ",
        " supports ",
        " requires ",
        " includes ",
        " excludes ",
        " offers ",
        " provides ",
        " runs ",
        " ranks ",
        "售价",
        "价格",
        "支持",
        "包含",
        "不含",
        "需要",
        "提供",
        "是",
        "为",
        "达到",
    ] {
        if low.contains(v) || clause.contains(v) {
            return true;
        }
    }
    false
}

/// Extract explicit inline citations (`[ref-1]`, `[1]`, `[unt_xxx]`, or `http(s)://...`).
pub fn extract_inline_citations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    // 1. Bracketed references: `[ref-1]`, `[1]`, `[unt_abcdef0123456789]`, `[E1]`
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(rel_end) = bytes[i + 1..].iter().position(|&b| b == b']')
        {
            let inner = text[i + 1..i + 1 + rel_end].trim();
            let is_ref_like = inner.starts_with("ref-")
                || inner.starts_with("unt_")
                || inner.starts_with("doc_")
                || inner.starts_with("http://")
                || inner.starts_with("https://")
                || (!inner.is_empty()
                    && inner.len() <= 8 // [策略] 短引用编号 `[1]` / `[E12]` 长度上限 8
                    && inner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && inner.bytes().any(|b| b.is_ascii_digit()));
            if is_ref_like && seen.insert(inner.to_owned()) {
                out.push(inner.to_owned());
            }
            i = i + 2 + rel_end;
            continue;
        }
        i += 1;
    }

    // 2. Bare HTTP(S) URLs
    for tok in text.split_whitespace() {
        let clean = tok.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '.' | '"' | '\''
            )
        });
        if (clean.starts_with("https://") || clean.starts_with("http://"))
            && seen.insert(clean.to_owned())
        {
            out.push(clean.to_owned());
        }
    }

    out
}

/// Extract conditional, exception, or scope-limiting qualifier clauses embedded in text.
pub fn extract_qualifier_clauses(text: &str, lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    let markers: &[&str] = &[
        "unless ",
        "except ",
        "excluding ",
        "only if ",
        "only for ",
        "provided that ",
        "subject to ",
        "capped at ",
        "up to ",
        "limited to ",
        "when billed ",
        "if billed ",
        "in benchmark",
        "in tests",
        "for enterprise",
        "除非",
        "除了",
        "不含",
        "仅限",
        "仅在",
        "在测试中",
        "前提是",
        "上限",
    ];
    for marker in markers {
        let pos = if marker.is_ascii() {
            lower.find(marker)
        } else {
            text.find(marker)
        };
        if let Some(p) = pos {
            let tail = text[p..].trim();
            let clause = tail
                .split(['.', ';', '。', '；'])
                .next()
                .unwrap_or(tail)
                .trim();
            if !clause.is_empty() && !out.iter().any(|q| q == clause) {
                out.push(clause.to_owned());
            }
        }
    }
    out
}

pub fn detect_claim_negation(lower: &str, raw: &str) -> bool {
    for tok in lower.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '\u{2019}') {
        let norm = tok.replace('\u{2019}', "'");
        if matches!(
            norm.as_str(),
            "not"
                | "never"
                | "no"
                | "none"
                | "neither"
                | "nor"
                | "cannot"
                | "can't"
                | "don't"
                | "doesn't"
                | "didn't"
                | "isn't"
                | "aren't"
                | "wasn't"
                | "weren't"
                | "won't"
                | "without"
                | "excludes"
                | "excluded"
                | "prohibited"
                | "forbidden"
                | "discontinued"
                | "unsupported"
        ) {
            return true;
        }
    }
    for cjk in [
        "不支持",
        "不包含",
        "不含",
        "没有",
        "从未",
        "并非",
        "不是",
        "禁止",
        "不可",
        "未提供",
        "已停用",
        "不存在",
    ] {
        if raw.contains(cjk) {
            return true;
        }
    }
    false
}

fn detect_quantifier(lower: &str, raw: &str) -> ClaimQuantifier {
    for pat in [
        "no public record",
        "no record",
        "no known",
        "does not exist",
        "do not exist",
        "never had",
        "none of",
        "zero incidents",
        "zero outages",
        "没有公开记录",
        "无公开记录",
        "不存在",
        "没有任何",
        "从未出现",
    ] {
        if lower.contains(pat) || raw.contains(pat) {
            return ClaimQuantifier::NegativeExistential;
        }
    }
    for pat in [
        "cheapest",
        "lowest price",
        "lowest cost",
        "highest ",
        "fastest",
        "slowest",
        "most expensive",
        "least expensive",
        "best ",
        "worst ",
        "the only ",
        "sole ",
        "number one",
        "#1",
        "ranked first",
        "market leader",
        "still leads",
        "still leading",
        "最便宜",
        "最低价",
        "价格最低",
        "最快",
        "最高",
        "唯一",
        "第一名",
        "领先",
    ] {
        if lower.contains(pat) || raw.contains(pat) {
            return ClaimQuantifier::Superlative;
        }
    }
    for pat in [
        "all platforms",
        "every platform",
        "all plans",
        "every plan",
        "all users",
        "every user",
        "all vendors",
        "always ",
        "across all ",
        "所有平台",
        "全部",
        "所有方案",
        "所有供应商",
        "总是",
    ] {
        if lower.contains(pat) || raw.contains(pat) {
            return ClaimQuantifier::Universal;
        }
    }
    ClaimQuantifier::Existential
}

pub fn detect_causal_assertion(lower: &str, raw: &str) -> bool {
    for pat in [
        " causes ",
        " caused ",
        " causing ",
        " leads to ",
        " led to ",
        " results in ",
        " resulted in ",
        " drives ",
        " drove ",
        " driven by ",
        " because of ",
        " directly caused ",
        " triggers ",
        " triggered ",
        " proves that ",
        " 导致",
        " 引起",
        " 造成",
        " 促使",
        " 引发",
        " 直接导致",
    ] {
        if lower.contains(pat) || raw.contains(pat) {
            return true;
        }
    }
    // Check CJK without leading space (Chinese sentences don't use spaces before verbs)
    for pat in ["导致", "引起", "造成", "引发", "促使", "归因于"] {
        if raw.contains(pat) {
            return true;
        }
    }
    false
}

fn extract_named_entity_anchors(text: &str, out: &mut BTreeSet<String>) {
    // Capture capitalized multi-word or single proper nouns / product designations (`Product A`, `Acme`, `Plan Pro`, `Vendor X`)
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0usize;
    while i < tokens.len() {
        let clean = tokens[i].trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if is_capitalized_entity_word(clean, i == 0) {
            // Check 2-token compound like `"Product A"`, `"Vendor X"`, `"Acme Pro"`, `"Plan Enterprise"`
            if i + 1 < tokens.len() {
                let nxt = tokens[i + 1]
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
                if !nxt.is_empty()
                    && (nxt.len() == 1
                        && nxt
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                        || is_capitalized_entity_word(nxt, false))
                {
                    out.insert(format!(
                        "ent:{} {}",
                        clean.to_ascii_lowercase(),
                        nxt.to_ascii_lowercase()
                    ));
                    i += 2;
                    continue;
                }
            }
            let clean_low = clean.to_ascii_lowercase();
            // [策略] 单个首字母大写实体词至少 3 字节，且排除单独出现的泛化产品层级/类别词避免跨 SKU 误串
            if clean.len() >= 3
                && !matches!(
                    clean_low.as_str(),
                    "pro"
                        | "lite"
                        | "plus"
                        | "max"
                        | "basic"
                        | "standard"
                        | "starter"
                        | "enterprise"
                        | "team"
                        | "plan"
                        | "tier"
                        | "service"
                        | "engine"
                        | "cloud"
                        | "storage"
                        | "cache"
                        | "system"
                        | "platform"
                )
            {
                out.insert(format!("ent:{clean_low}"));
            }
        }
        i += 1;
    }
}

fn is_capitalized_entity_word(tok: &str, is_sentence_start: bool) -> bool {
    let mut chars = tok.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    let low = tok.to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "the"
            | "this"
            | "that"
            | "these"
            | "those"
            | "there"
            | "it"
            | "in"
            | "on"
            | "at"
            | "by"
            | "for"
            | "with"
            | "from"
            | "all"
            | "every"
            | "no"
            | "not"
            | "if"
            | "when"
            | "while"
            | "however"
            | "overall"
            | "therefore"
            | "thus"
            | "according"
            | "currently"
            | "today"
            | "usd"
            | "eur"
            | "gbp"
            | "cny"
            | "jpy"
    ) {
        return false;
    }
    if is_sentence_start {
        // [策略] 句首大写词含内部大写/数字、属于产品角色词或长度 >= 4 时保留为候选实体
        tok.chars()
            .skip(1)
            .any(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            || matches!(
                low.as_str(),
                "product" | "vendor" | "plan" | "tier" | "model" | "service" | "company" | "acme"
            )
            || tok.len() >= 4
    } else {
        true
    }
}

fn classify_verifiability(
    raw: &str,
    lower: &str,
    term_freqs: &std::collections::BTreeMap<String, u32>,
) -> (bool, Option<String>) {
    // [策略] 少于 4 字节或无实质词项的碎片不具备事实可核验性
    if raw.len() < 4 || term_freqs.is_empty() {
        return (false, Some("empty_or_trivial_fragment".to_owned()));
    }
    for subj in [
        "i think ",
        "i believe ",
        "in my opinion",
        "in conclusion",
        "overall, it feels",
        "probably looks nice",
        "to summarize",
        "我认为",
        "我觉得",
        "总体而言",
        "综上所述",
    ] {
        if lower.starts_with(subj) || raw.starts_with(subj) {
            return (false, Some("subjective_or_rhetorical_statement".to_owned()));
        }
    }
    (true, None)
}

/// Focus a multi-sentence non-table paragraph onto the sentence most aligned with `claim_text`,
/// while preserving an immediately following exception/qualification sentence (`However, ...`,
/// `Unless ...`, `Except ...`, `Note: ...`) so adjacent qualifiers are never dropped while unrelated
/// sibling negations in the same paragraph cannot contaminate polarity checks.
pub(crate) fn focus_paragraph_sentence(
    claim_text: &str,
    claim_entities: &[String],
    unit_text: &str,
) -> String {
    let sents = split_sentences_preserving_context(unit_text);
    if sents.len() <= 1 {
        return unit_text.to_owned();
    }
    let qa = crate::context::tokenize_unified::analyze_query(claim_text);
    let mut best_idx = 0usize;
    let mut best_score = (0usize, 0usize);
    for (i, s) in sents.iter().enumerate() {
        let low = s.to_ascii_lowercase();
        let ent_hits = claim_entities
            .iter()
            .filter(|e| low.contains(e.as_str()))
            .count();
        let lex = analyze_text(s);
        let term_hits = qa
            .lexical_concepts
            .iter()
            .filter(|(st, alt)| {
                lex.term_freqs.contains_key(st)
                    || alt
                        .as_deref()
                        .is_some_and(|a| lex.term_freqs.contains_key(a))
            })
            .count();
        let score = (term_hits, ent_hits);
        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }
    if best_score == (0, 0) {
        return unit_text.to_owned();
    }
    let mut focused = sents[best_idx].clone();
    if let Some(next_s) = sents.get(best_idx + 1) {
        let nl = next_s.trim_start().to_ascii_lowercase();
        let is_followup_qualifier = nl.starts_with("however")
            || nl.starts_with("except")
            || nl.starts_with("unless")
            || nl.starts_with("note:")
            || nl.starts_with("caveat:")
            || nl.starts_with("limitation:")
            || nl.starts_with("only for")
            || next_s.trim_start().starts_with("但是")
            || next_s.trim_start().starts_with("不过")
            || next_s.trim_start().starts_with("除非")
            || next_s.trim_start().starts_with("注意");
        if is_followup_qualifier {
            focused.push(' ');
            focused.push_str(next_s);
        }
    }
    focused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_qualifiers_citations_and_identifies_quantifiers_and_causality() {
        let ans = "Product A costs $99/mo unless billed annually [ref-1]. \
                   Vendor X is the cheapest provider at $49/mo. \
                   Rising power prices caused data center investment to drop in 2026.";
        let claims = decompose_claims(ans, &[]);
        assert_eq!(claims.len(), 3);
        assert_eq!(claims[0].cited_refs, vec!["ref-1"]);
        assert!(!claims[0].qualifiers.is_empty());
        assert_eq!(claims[1].quantifier, ClaimQuantifier::Superlative);
        assert!(claims[2].asserts_causality);
        assert_eq!(claims[2].asserted_years, vec![2026]);
    }
}
