//! 标题和候选小标题是不是同一个标题：规范化（小写、只留字母数字）后包含，或者小标题的词
//! 大多出现在标题里。同一个页面标题要和页上每个候选比较，所以规范化形式只建一次。

/// Lowercase/alphanumeric form of a page title. The same title is compared
/// against every candidate heading on the page, so building the normalized
/// form and word list once avoids a lowercase allocation plus a word-set
/// allocation for each heading.
pub(crate) struct TitleSignature {
    normalized: String,
    words: Vec<String>,
    /// `words` 里不同的词有几个。
    distinct: usize,
}

impl TitleSignature {
    pub(crate) fn new(title: &str) -> Self {
        let lower = title.to_lowercase();
        let normalized = lower
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>();
        let words: Vec<String> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.chars().count() > 2)
            .map(str::to_owned)
            .collect();
        let distinct = words
            .iter()
            .enumerate()
            .filter(|(i, w)| !words[..*i].contains(w))
            .count();
        Self {
            normalized,
            words,
            distinct,
        }
    }
}

pub(crate) fn title_agrees_with(signature: &TitleSignature, heading: &str) -> bool {
    if heading.is_ascii() {
        return title_agrees_ascii(signature, heading);
    }
    title_agrees_allocating(signature, heading)
}

/// ASCII fast path: normalize the heading into a stack buffer instead of
/// allocating a lowercase `String` plus a second normalized `String` for every
/// candidate heading on the page.
fn title_agrees_ascii(signature: &TitleSignature, heading: &str) -> bool {
    let mut needle = [0u8; 128];
    let mut len = 0usize;
    for byte in heading.bytes() {
        let byte = byte.to_ascii_lowercase();
        if !byte.is_ascii_alphanumeric() {
            continue;
        }
        if len == needle.len() {
            return title_agrees_allocating(signature, heading);
        }
        needle[len] = byte;
        len += 1;
    }
    if len >= 8 {
        let needle = std::str::from_utf8(&needle[..len]).expect("ASCII buffer");
        if signature.normalized.contains(needle) {
            return true;
        }
    }
    // Deduplicate heading words the way the old HashSet did, without hashing
    // or per-word allocation: headings have only a handful of words.
    let mut heading_words = [""; 16];
    let mut count = 0usize;
    for word in heading.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.len() <= 2 {
            continue;
        }
        if heading_words[..count]
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(word))
        {
            continue;
        }
        if count == heading_words.len() {
            break;
        }
        heading_words[count] = word;
        count += 1;
    }
    if count < 2 {
        return false;
    }
    let matches = heading_words[..count]
        .iter()
        .filter(|word| {
            signature
                .words
                .iter()
                .any(|title_word| title_word.eq_ignore_ascii_case(word))
        })
        .count();
    matches * 5 >= count * 4
}

fn title_agrees_allocating(signature: &TitleSignature, heading: &str) -> bool {
    // 整串转小写只有希腊大写 Σ 看上下文（词尾 ς、别处 σ），其余逐字转换和整串结果相同
    if heading.contains('Σ') {
        return title_agrees_whole(signature, heading);
    }
    // 一遍流式：逐字转小写，同时拼规范化串（只留字母数字）、切词去重。规范化串比标题的还长时
    // 不可能是它的子串；那时不同词再超过上界（见 title_agrees_whole）就已经确定不一致，不用
    // 再往下看（没闭合的 <h2> 吞掉整页时，以前要先把整页转小写、拼一遍）。
    let mut normalized = String::new();
    let mut normalized_chars = 0usize;
    let mut too_long = false;
    let mut word = String::new();
    let mut word_chars = 0usize;
    let mut heading_words: Vec<String> = Vec::new();
    fn end_word(word: &mut String, word_chars: &mut usize, words: &mut Vec<String>) {
        if *word_chars > 2 && !words.iter().any(|w| w == word) {
            words.push(std::mem::take(word));
        } else {
            word.clear();
        }
        *word_chars = 0;
    }
    for c in heading.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            normalized_chars += 1;
            if !too_long {
                normalized.push(c);
                too_long = normalized.len() > signature.normalized.len();
            }
            word.push(c);
            word_chars += 1;
        } else {
            end_word(&mut word, &mut word_chars, &mut heading_words);
            if too_long && heading_words.len() * 4 > signature.distinct * 5 {
                return false;
            }
        }
    }
    end_word(&mut word, &mut word_chars, &mut heading_words);
    if !too_long && normalized_chars >= 8 && signature.normalized.contains(&normalized) {
        return true;
    }
    if heading_words.len() < 2 || heading_words.len() * 4 > signature.distinct * 5 {
        return false;
    }
    let matches = heading_words
        .iter()
        .filter(|word| signature.words.iter().any(|title_word| title_word == *word))
        .count();
    matches * 5 >= heading_words.len() * 4
}

/// 整串转小写的版本（标题里有希腊大写 Σ 时用）。
fn title_agrees_whole(signature: &TitleSignature, heading: &str) -> bool {
    let heading = heading.to_lowercase();
    let normalized = heading
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    let normalized_len = if normalized.is_ascii() {
        normalized.len()
    } else {
        normalized.chars().count()
    };
    if normalized_len >= 8 && signature.normalized.contains(&normalized) {
        return true;
    }
    // Deduplicate heading words the way the old HashSet did, but without
    // hashing or per-word allocation: headings have only a handful of words.
    // 一致要求至少八成的不同词出现在标题里，而能匹配上的最多是标题的不同词数：
    // 不同词超过它的 5/4 就不可能一致，不用再数（一个没闭合的 <h2> 吞掉整页时有几千个词，
    // 逐个查重是平方级的）。
    let mut heading_words: Vec<&str> = Vec::new();
    for word in heading.split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() > 2 && !heading_words.contains(&word) {
            heading_words.push(word);
            if heading_words.len() * 4 > signature.distinct * 5 {
                return false;
            }
        }
    }
    if heading_words.len() < 2 {
        return false;
    }
    let matches = heading_words
        .iter()
        .filter(|word| signature.words.iter().any(|title_word| title_word == *word))
        .count();
    matches * 5 >= heading_words.len() * 4
}

#[cfg(test)]
pub(crate) fn title_agrees(title: &str, heading: &str) -> bool {
    title_agrees_with(&TitleSignature::new(title), heading)
}

#[cfg(test)]
mod title_tests {
    use super::{
        TitleSignature, title_agrees, title_agrees_allocating, title_agrees_ascii,
        title_agrees_whole,
    };

    #[test]
    fn title_agrees_matches_normalized_containment() {
        assert!(title_agrees(
            "Global Market Tech Breakthrough News Item #1",
            "Global Market Tech Breakthrough News Item #1"
        ));
    }

    #[test]
    fn title_agrees_counts_unique_heading_words() {
        // Duplicate heading words must collapse exactly like the old HashSet.
        assert!(title_agrees("alpha beta gamma", "alpha alpha alpha gamma"));
        assert!(!title_agrees("alpha beta gamma", "alpha alpha alpha delta"));
    }

    #[test]
    fn title_agrees_rejects_unrelated_heading() {
        assert!(!title_agrees(
            "Global Market News",
            "Completely Different Topic"
        ));
    }

    #[test]
    fn ascii_fast_path_matches_allocating_path() {
        let titles = [
            "Global Market Tech Breakthrough News Item #1",
            "alpha beta gamma",
            "A",
            "Short",
            "Café résumé naïve",
        ];
        let headings = [
            "Global Market Tech Breakthrough News Item #1",
            "GLOBAL market tech",
            "alpha alpha alpha gamma",
            "alpha beta delta",
            "a",
            "ab cd",
            "Café résumé",
            "naïve café",
            "",
        ];
        for title in titles {
            let signature = TitleSignature::new(title);
            for heading in headings {
                if !heading.is_ascii() {
                    continue;
                }
                assert_eq!(
                    title_agrees_ascii(&signature, heading),
                    title_agrees_allocating(&signature, heading),
                    "title={title:?} heading={heading:?}"
                );
            }
        }
    }

    #[test]
    fn streaming_comparison_matches_the_whole_string_one() {
        const ATOMS: &[&str] = &[
            "Global",
            "market",
            "MARKET",
            "tech",
            " ",
            "-",
            ",",
            "İstanbul",
            "ΣΟΦΙΑ",
            "σοφίας",
            "café",
            "Café",
            "中文标题",
            "标题",
            "ab",
            "x",
            "123",
            "news",
            "item",
            "#1",
            "\u{307}",
            "ǅ",
            "Ⅻ",
            "straße",
            "STRASSE",
        ];
        let titles = [
            "Global Market Tech Breakthrough News Item #1",
            "Café résumé naïve",
            "中文标题 新闻",
            "İstanbul Σοφία market",
            "",
            "ab cd",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for title in titles {
            let signature = TitleSignature::new(&title.to_lowercase());
            for _ in 0..4000 {
                let n = (next() % 40) as usize;
                let heading: String = (0..n)
                    .map(|_| ATOMS[(next() % ATOMS.len() as u64) as usize])
                    .collect::<Vec<_>>()
                    .join(if next() % 2 == 0 { " " } else { "" });
                assert_eq!(
                    title_agrees_allocating(&signature, &heading),
                    title_agrees_whole(&signature, &heading),
                    "title={title:?} heading={heading:?}"
                );
            }
        }
    }

    #[test]
    fn non_ascii_heading_still_uses_the_allocating_path() {
        assert!(title_agrees("Café résumé naïve", "café résumé naïve"));
        assert!(!title_agrees("Café résumé naïve", "完全不同的标题"));
    }
}
