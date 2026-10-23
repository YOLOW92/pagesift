//! TeX 语法树构建与公式排版逻辑。

use super::symbols::{ACCENTS, FUNCS, is_combining, is_greek, is_math_symbol, push_char};
use crate::font::MathKind;
use crate::interp::Glyph;

/// 一个字形和它规范化后的文字。
pub(crate) struct Item<'a> {
    pub(crate) g: &'a Glyph,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Word {
    Text,
    Glue,
    Math,
}

pub(crate) fn classify(word: &[Item<'_>]) -> Word {
    let chars: Vec<char> = word
        .iter()
        .flat_map(|i| i.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    if chars.is_empty() {
        return Word::Glue;
    }
    let math_font = word
        .iter()
        .any(|i| i.g.math != MathKind::Text && !i.text.trim().is_empty());
    let symbol = chars.iter().any(|&c| is_math_symbol(c));
    let short_greek = chars.len() <= 3 && chars.iter().any(|&c| is_greek(c));
    if math_font || symbol || short_greek {
        return Word::Math;
    }
    if chars
        .iter()
        .all(|&c| c.is_ascii_digit() || "+-=<>()[]{}|/,.:;!'*−".contains(c))
    {
        return Word::Glue;
    }
    Word::Text
}

pub(crate) fn evidence(item: &Item<'_>) -> bool {
    let c = item.text.trim().chars().next();
    match item.g.math {
        MathKind::Text => false,
        MathKind::Symbol => c.is_some_and(|c| {
            c.is_alphanumeric() || is_math_symbol(c) || "=+<>|()[]{}′'".contains(c)
        }),
        _ => c.is_some(),
    }
}

pub(crate) fn punct_only(word: &[Item<'_>]) -> bool {
    word.iter()
        .all(|i| i.text.trim().chars().all(|c| ",.;:".contains(c)))
}

pub(crate) fn push_word(out: &mut String, w: &str) {
    if w.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(w);
}

pub(crate) fn plain(word: &[Item<'_>]) -> String {
    word.iter().map(|i| i.text.trim_start()).collect()
}

pub(crate) fn plain_items(items: &[&Item<'_>]) -> String {
    items.iter().map(|i| i.text.trim_start()).collect()
}

pub(crate) struct Atom {
    pub(crate) tex: String,
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    pub(crate) y: f64,
    pub(crate) size: f64,
    pub(crate) fixed: bool,
}

pub(crate) fn tex_of(items: &[&Item<'_>], base_y: f64, base_size: f64) -> String {
    let mut atoms: Vec<Atom> = Vec::new();
    let mut accents: Vec<(usize, &str, f64)> = Vec::new();
    let mut k = 0;
    while k < items.len() {
        let item = items[k];
        k += 1;
        let t = item.text.trim();
        if t.is_empty() {
            continue;
        }
        let g = item.g;
        if g.math == MathKind::Text && t.chars().all(|c| c.is_ascii_alphabetic()) {
            let mut j = k;
            let mut name = t.to_string();
            while j < items.len()
                && items[j].g.math == MathKind::Text
                && (items[j].g.y - g.y).abs() < 0.1 * g.size
                && items[j]
                    .text
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphabetic())
                && !items[j].text.trim().is_empty()
            {
                name.push_str(items[j].text.trim());
                j += 1;
            }
            if FUNCS.contains(&name.as_str()) {
                atoms.push(Atom {
                    tex: format!("\\{name} "),
                    x0: g.x,
                    x1: items[j - 1].g.x_end,
                    y: g.y,
                    size: g.size,
                    fixed: false,
                });
                k = j;
                continue;
            }
        }
        let mut chars = t.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && (g.math != MathKind::Extension || is_combining(c))
            && let Some((_, cmd)) = ACCENTS.iter().find(|(a, _)| *a == c)
        {
            accents.push((atoms.len(), cmd, (g.x + g.x_end) / 2.0));
            continue;
        }
        let mut tex = String::new();
        for c in t.chars() {
            push_char(c, g.math, &mut tex);
        }
        atoms.push(Atom {
            tex,
            x0: g.x,
            x1: g.x_end.max(g.x),
            y: g.y,
            size: g.size,
            fixed: g.math == MathKind::Extension,
        });
    }
    for (at, cmd, cx) in accents.into_iter().rev() {
        let pick = [at, at.wrapping_sub(1)]
            .into_iter()
            .filter(|&k| k < atoms.len())
            .find(|&k| cx >= atoms[k].x0 - 0.5 && cx <= atoms[k].x1 + 0.5);
        if let Some(k) = pick {
            atoms[k].tex = format!("{cmd}{{{}}}", atoms[k].tex.trim());
        }
    }
    let mut out = String::new();
    build(&atoms, base_y, base_size, &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn level(a: &Atom, base_y: f64, base_size: f64) -> i32 {
    if a.fixed || a.size > 0.85 * base_size {
        return 0;
    }
    let dy = a.y - base_y;
    if dy > 0.12 * base_size {
        1
    } else if dy < -0.08 * base_size {
        -1
    } else {
        0
    }
}

pub(crate) fn build(atoms: &[Atom], base_y: f64, base_size: f64, out: &mut String) {
    let mut i = 0;
    let mut have_base = false;
    let (mut has_sup, mut has_sub) = (false, false);
    while i < atoms.len() {
        let lvl = level(&atoms[i], base_y, base_size);
        if lvl == 0 {
            out.push_str(&atoms[i].tex);
            have_base = true;
            has_sup = atoms[i].tex.ends_with('\'');
            has_sub = false;
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < atoms.len() && level(&atoms[j], base_y, base_size) == lvl {
            j += 1;
        }
        let group = &atoms[i..j];
        if lvl > 0 && group.iter().all(|a| a.tex.trim() == "'") && !has_sup {
            out.push_str(&"'".repeat(group.len()));
            has_sup = true;
        } else {
            let repeated = if lvl > 0 { has_sup } else { has_sub };
            if !have_base || repeated {
                out.push_str("{}");
                has_sup = false;
                has_sub = false;
            }
            if lvl > 0 {
                has_sup = true;
            } else {
                has_sub = true;
            }
            let inner = group
                .iter()
                .max_by(|a, b| {
                    a.size
                        .total_cmp(&b.size)
                        .then((b.y - base_y).abs().total_cmp(&(a.y - base_y).abs()))
                })
                .map_or((group[0].y, group[0].size), |a| (a.y, a.size));
            let mut sub = String::new();
            build(group, inner.0, inner.1, &mut sub);
            out.push_str(if lvl > 0 { "^{" } else { "_{" });
            out.push_str(sub.trim());
            out.push('}');
        }
        i = j;
    }
}

/// 一行的文字。没有公式时返回 None，调用方照普通文字拼。
pub(crate) fn line_text(words: &[Vec<Item<'_>>], base_y: f64, base_size: f64) -> Option<String> {
    let kinds: Vec<Word> = words.iter().map(|w| classify(w)).collect();
    if !kinds.contains(&Word::Math) {
        return None;
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if kinds[i] != Word::Math {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 && kinds[start - 1] == Word::Glue {
            start -= 1;
        }
        let mut end = i + 1;
        loop {
            let mut j = end;
            while j < words.len() && kinds[j] == Word::Glue {
                j += 1;
            }
            if j < words.len() && kinds[j] == Word::Math {
                end = j + 1;
            } else {
                end = j;
                break;
            }
        }
        while start < end && punct_only(&words[start]) {
            start += 1;
        }
        while end > start && punct_only(&words[end - 1]) {
            end -= 1;
        }
        if words[start..end].iter().flatten().any(evidence) {
            spans.push((start, end));
        }
        i = end.max(i + 1);
    }
    if spans.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut w = 0;
    for (start, end) in spans {
        while w < start {
            push_word(&mut out, &plain(&words[w]));
            w += 1;
        }
        let items: Vec<&Item> = words[start..end].iter().flatten().collect();
        let mut cut = items.len();
        while cut > 0 && matches!(items[cut - 1].text.trim(), "," | "." | ";" | ":") {
            cut -= 1;
        }
        let tail: String = items[cut..].iter().map(|i| i.text.trim()).collect();
        let mut parts: Vec<String> = Vec::new();
        let mut taken = 0;
        for word in &words[start..end] {
            let n = word.len().min(cut.saturating_sub(taken));
            let refs: Vec<&Item> = word[..n].iter().collect();
            taken += word.len();
            let tex = tex_of(&refs, base_y, base_size);
            if !tex.is_empty() {
                parts.push(tex);
            }
        }
        let tex = parts.join(" ");
        if tex.trim().is_empty() {
            push_word(&mut out, &plain_items(&items));
        } else {
            push_word(&mut out, &format!("${}${tail}", tex.trim()));
        }
        w = end;
    }
    while w < words.len() {
        push_word(&mut out, &plain(&words[w]));
        w += 1;
    }
    Some(out)
}
