//! Bounded, evidence-bearing technology/page/secret indicators. These are
//! heuristic findings, not provider validation or a vulnerability verdict.
use std::collections::BTreeSet;

use pagesift_html::Html;
use regex::{Regex, RegexBuilder};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Technology,
    SecretIndicator,
    PageKind,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub rule: String,
    pub kind: FindingKind,
    pub source: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub redacted: bool,
}

pub struct EvidenceRule {
    pub name: String,
    pub kind: FindingKind,
    regex: Regex,
}
impl EvidenceRule {
    pub fn new(name: &str, kind: FindingKind, pattern: &str) -> Result<Self, String> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            || pattern.len() > 4096
        {
            return Err("invalid evidence rule".into());
        }
        let regex = RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            name: name.into(),
            kind,
            regex,
        })
    }
}

pub struct KnowledgeExtractor {
    rules: Vec<EvidenceRule>,
}
#[derive(Debug, Serialize)]
pub struct Knowledge {
    pub findings: Vec<Finding>,
    pub incomplete: bool,
}
impl KnowledgeExtractor {
    pub fn new(rules: Vec<EvidenceRule>) -> Result<Self, String> {
        if rules.len() > 128 {
            return Err("at most 128 evidence rules".into());
        }
        let mut names = BTreeSet::new();
        if rules.iter().any(|r| !names.insert(&r.name)) {
            return Err("duplicate rule names".into());
        }
        Ok(Self { rules })
    }
    pub fn builtin() -> Self {
        use FindingKind::{SecretIndicator as S, Technology as T};
        Self::new([
            ("wordpress", T, r#"(?i)(?:/wp-(?:content|includes)/|name\s*=\s*["']generator["'][^>]*content\s*=\s*["']WordPress)"#),
            ("nextjs", T, r"(?:/_next/static/|__NEXT_DATA__)"),
            ("drupal", T, r#"(?i)(?:Drupal\.settings|drupalSettings|content\s*=\s*["']Drupal)"#),
            ("nginx", T, r"(?im)^server:\s*nginx(?:/[^\r\n ]*)?"),
            ("aws_access_key_id", S, r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
            ("github_token", S, r"\b(?:gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{40,255})\b"),
            ("private_key", S, r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----"),
            ("stripe_secret", S, r"\bsk_(?:live|test)_[A-Za-z0-9]{16,128}\b"),
            ("slack_token", S, r"\bxox[baprs]-[A-Za-z0-9-]{20,200}\b"),
        ].into_iter().map(|(n,k,p)| EvidenceRule::new(n,k,p).expect("static evidence regex")).collect()).expect("static evidence rules")
    }
    /// Values never appear in results, including custom rules. Byte ranges are
    /// relative to the explicitly named input. Headers are allowlisted to avoid
    /// exporting/processing Set-Cookie and Authorization by accident.
    pub fn extract(&self, html: &str, headers: &[(String, String)]) -> Result<Knowledge, String> {
        if html.len() > 4 * 1024 * 1024
            || headers.len() > 128
            || headers.iter().any(|(k, v)| k.len() + v.len() > 8192)
        {
            return Err("knowledge input exceeds budget".into());
        }
        let mut out = Knowledge {
            findings: Vec::new(),
            incomplete: false,
        };
        self.scan("body", html, &mut out);
        for (i, (name, value)) in headers.iter().enumerate() {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "server" | "x-powered-by" | "content-type"
            ) {
                self.scan(
                    &format!("header:{i}"),
                    &format!("{}: {value}", name.to_ascii_lowercase()),
                    &mut out,
                );
            }
        }
        let doc = Html::parse(html);
        let login = doc.root().descendants().any(|n| {
            n.as_element().is_some_and(|e| {
                e.name() == "input"
                    && e.attr("type")
                        .is_some_and(|s| s.eq_ignore_ascii_case("password"))
            })
        });
        if login && out.findings.len() < 512 {
            out.findings.push(Finding {
                rule: "login_form".into(),
                kind: FindingKind::PageKind,
                source: "body".into(),
                byte_start: 0,
                byte_end: 0,
                redacted: true,
            });
        }
        Ok(out)
    }
    fn scan(&self, source: &str, text: &str, out: &mut Knowledge) {
        for rule in &self.rules {
            for m in rule.regex.find_iter(text) {
                if out.findings.len() >= 512 {
                    out.incomplete = true;
                    return;
                }
                out.findings.push(Finding {
                    rule: rule.name.clone(),
                    kind: rule.kind,
                    source: source.into(),
                    byte_start: m.start(),
                    byte_end: m.end(),
                    redacted: true,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn findings_are_redacted_and_have_provenance() {
        let body =
            "<script src='/wp-content/a.js'></script><input type='PASSWORD'> AKIAABCDEFGHIJKLMNOP";
        let report = KnowledgeExtractor::builtin()
            .extract(
                body,
                &[
                    ("Server".into(), "nginx/1.25".into()),
                    ("Set-Cookie".into(), "sk_live_aaaaaaaaaaaaaaaaaaaa".into()),
                ],
            )
            .unwrap();
        let names: Vec<_> = report.findings.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(
            names,
            ["wordpress", "aws_access_key_id", "nginx", "login_form"]
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("ABCDEFGHIJKLMNOP"));
        assert!(report.findings.iter().all(|f| f.redacted));
        assert_eq!(
            &body[report.findings[1].byte_start..report.findings[1].byte_end],
            "AKIAABCDEFGHIJKLMNOP"
        );
    }
    #[test]
    fn custom_rules_and_budgets() {
        let rule =
            EvidenceRule::new("custom", FindingKind::SecretIndicator, r"token-[a-z]+").unwrap();
        let report = KnowledgeExtractor::new(vec![rule])
            .unwrap()
            .extract(&"token-secret ".repeat(600), &[])
            .unwrap();
        assert_eq!(report.findings.len(), 512);
        assert!(report.incomplete);
        assert!(EvidenceRule::new("bad name", FindingKind::Technology, "x").is_err());
        assert!(EvidenceRule::new("bad", FindingKind::Technology, "(").is_err());
    }
}
