//! 每个字段来自哪个来源：模型引用的证据要经过核对，字段值逐词出现在证据文字里才算 `verified`。
//! 模型引用错了而别的证据里有这个值时，改成真正含有它的证据；哪里都没有时保留引用，标成未核实。

use std::collections::BTreeMap;

use pagesift_api::types::{AgentEvidence, FieldSource};
use serde_json::{Map, Value};

/// 去掉数字中间的千分位逗号再分词，前后加空格，好按整词比较（`12` 不会命中 `312`）。
fn normalize(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let kept: String = chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            !(c == ','
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        .map(|(_, &c)| c)
        .collect();
    format!(" {} ", pagesift_extract::tokenize(&kept, false).join(" "))
}

/// 叶子字段：`(JSON Pointer, 值)`。空值表示这个字段没有内容，不需要来源。
fn leaves<'a>(v: &'a Value, path: String, out: &mut Vec<(String, &'a Value)>) {
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                let k = k.replace('~', "~0").replace('/', "~1");
                leaves(c, format!("{path}/{k}"), out);
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                leaves(c, format!("{path}/{i}"), out);
            }
        }
        Value::Null => {}
        _ => out.push((path, v)),
    }
}

/// 模型为这个字段引用的证据 id：自己的，或者最近的上级（引用了整个 `/founders/0` 时对下面每个字段都算）。
fn cited_for<'a>(path: &str, cited: &'a Map<String, Value>) -> Vec<&'a str> {
    let mut p = path;
    loop {
        if let Some(v) = cited.get(p) {
            let ids = match v {
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                Value::String(s) => vec![s.as_str()],
                _ => Vec::new(),
            };
            if !ids.is_empty() {
                return ids;
            }
        }
        match p.rfind('/') {
            Some(i) => p = &p[..i],
            None => return Vec::new(),
        }
    }
}

/// 算出每个字段的来源，同时返回没有核实的字段（JSON Pointer）。
pub(super) fn attribute(
    data: &Value,
    cited: &Map<String, Value>,
    evidence: &[AgentEvidence],
) -> (BTreeMap<String, FieldSource>, Vec<String>) {
    let haystacks: Vec<String> = evidence.iter().map(|e| normalize(&e.text)).collect();
    let mut fields = Vec::new();
    leaves(data, String::new(), &mut fields);
    let mut out = BTreeMap::new();
    let mut unverified = Vec::new();
    for (path, value) in fields {
        // 模型可能直接写网址：和证据 id 一样认
        let cited_ids: Vec<usize> = cited_for(&path, cited)
            .into_iter()
            .flat_map(|c| {
                evidence
                    .iter()
                    .enumerate()
                    .filter(move |(_, e)| e.id == c || e.url == c)
                    .map(|(i, _)| i)
            })
            .collect();
        let text = match value {
            Value::String(s) => Some(normalize(s)),
            Value::Number(n) => Some(normalize(&n.to_string())),
            _ => None, // 布尔值没法在文字里核对
        };
        let hits: Vec<usize> = match &text {
            Some(t) if t.trim().is_empty() => Vec::new(),
            Some(t) => haystacks
                .iter()
                .enumerate()
                .filter(|(_, h)| h.contains(t.as_str()))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        let confirmed: Vec<usize> = cited_ids
            .iter()
            .copied()
            .filter(|i| hits.contains(i))
            .collect();
        let (chosen, verified) = if !confirmed.is_empty() {
            (confirmed, true)
        } else if !hits.is_empty() {
            (hits, true)
        } else {
            (cited_ids, false)
        };
        if !verified && !matches!(value, Value::Bool(_)) {
            unverified.push(path.clone());
        }
        let mut sources: Vec<String> = chosen.iter().map(|&i| evidence[i].url.clone()).collect();
        sources.sort();
        sources.dedup();
        out.insert(
            path,
            FieldSource {
                sources,
                evidence_ids: chosen.iter().map(|&i| evidence[i].id.clone()).collect(),
                verified,
            },
        );
    }
    (out, unverified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(id: &str, url: &str, text: &str) -> AgentEvidence {
        AgentEvidence {
            id: id.into(),
            url: url.into(),
            kind: "page".into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn checks_citations_against_the_text() {
        let evidence = [
            ev(
                "E1",
                "https://a.test/",
                "Founded in 1999 by Ada. Revenue 1,234 EUR.",
            ),
            ev("E2", "https://b.test/", "The office is in Oslo."),
        ];
        let data = json!({
            "founder": "Ada",
            "city": "Oslo",
            "revenue": 1234,
            "ceo": "Grace",
            "open": true,
            "nothing": null,
            "people": [{"name": "Ada"}],
        });
        let cited = json!({
            "/founder": ["E1"],
            "/city": ["E1"],         // 引用错了：其实在 E2
            "/ceo": ["E2"],          // 哪里都没有
            "/people/0": "E1",       // 引用上级
            "/open": ["E2"],
        });
        let (f, unverified) = attribute(&data, cited.as_object().unwrap(), &evidence);
        assert!(f["/founder"].verified);
        assert_eq!(f["/founder"].sources, ["https://a.test/"]);
        assert_eq!(f["/city"].sources, ["https://b.test/"]);
        assert!(f["/city"].verified);
        assert!(f["/revenue"].verified, "千分位逗号不影响比较");
        assert!(!f["/ceo"].verified);
        assert_eq!(f["/ceo"].evidence_ids, ["E2"]);
        assert!(f["/people/0/name"].verified);
        assert!(!f["/open"].verified);
        assert_eq!(f["/open"].sources, ["https://b.test/"]);
        assert!(!f.contains_key("/nothing"));
        assert_eq!(unverified, ["/ceo"]);
    }

    #[test]
    fn matches_whole_words_only() {
        let evidence = [ev("E1", "https://a.test/", "Room 312, opened 1999")];
        let (f, un) = attribute(&json!({"n": 12}), &Map::new(), &evidence);
        assert!(!f["/n"].verified);
        assert_eq!(un, ["/n"]);
        let cjk = [ev("E1", "https://a.test/", "澄迈加快农村生活污水治理")];
        let (f, _) = attribute(&json!({"t": "污水治理"}), &Map::new(), &cjk);
        assert!(f["/t"].verified);
    }
}
