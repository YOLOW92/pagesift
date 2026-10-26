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
    /// 正则命中后的二次校验（regex crate 不支持 lookahead，"同时含数字和字母"这类条件放这里）。
    check: Option<fn(&str) -> bool>,
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
            check: None,
        })
    }

    fn with_check(mut self, check: fn(&str) -> bool) -> Self {
        self.check = Some(check);
        self
    }
}

/// OpenAI 密钥体（去掉 `sk-` 和 `proj-` 这类前缀后）必须同时含数字和字母：CSS 变量名
/// （`sk-color-icon-secondary`）和 URL slug（`sk-hynix-reports-record-profit`）通常没有数字。
fn openai_key_body_ok(m: &str) -> bool {
    let body = m.strip_prefix("sk-").unwrap_or(m);
    let body = ["proj-", "svcacct-", "admin-", "ant-"]
        .iter()
        .find_map(|p| body.strip_prefix(p))
        .unwrap_or(body);
    body.bytes().any(|c| c.is_ascii_digit()) && body.bytes().any(|c| c.is_ascii_alphabetic())
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
            // 新格式 sk-proj-/sk-svcacct-/sk-admin-（以及原有的 sk-ant-）后跟 ≥20 位；旧格式 sk- 后跟
            // ≥32 位纯字母数字（不含连字符，排除 CSS 变量名和 URL slug）。另有数字+字母的后置校验。
            ("openai_key", S, r"\bsk-(?:(?:proj|svcacct|admin|ant)-[A-Za-z0-9_-]{20,}|[A-Za-z0-9]{32,})\b"),
            ("jwt", S, r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
            ("api_key_assignment", S, r#"(?i)\bapi[_-]?key\s*[:=]\s*["'][A-Za-z0-9_\-]{16,}["']"#),
            ("slack_token", S, r"\bxox[baprs]-[A-Za-z0-9-]{20,200}\b"),
        ].into_iter().map(|(n,k,p)| {
            let rule = EvidenceRule::new(n,k,p).expect("static evidence regex");
            if n == "openai_key" { rule.with_check(openai_key_body_ok) } else { rule }
        }).collect()).expect("static evidence rules")
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
                if rule.check.is_some_and(|check| !check(m.as_str())) {
                    continue;
                }
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

    fn openai_hits(body: &str) -> usize {
        KnowledgeExtractor::builtin()
            .extract(body, &[])
            .unwrap()
            .findings
            .iter()
            .filter(|f| f.rule == "openai_key")
            .count()
    }

    #[test]
    fn openai_key_matches_real_formats() {
        assert_eq!(openai_hits("key = 'sk-proj-FAKEKEY1234567890abcdef'"), 1);
        assert_eq!(openai_hits("sk-svcacct-Ab3_defghijklmnopqrstu-vw"), 1);
        assert_eq!(openai_hits("sk-admin-0123456789abcdefghijKLMN"), 1);
        assert_eq!(
            openai_hits("OPENAI_API_KEY=sk-AbCdEf0123456789AbCdEf0123456789AbCdEf0123456789"),
            1
        );
        // Stripe 走另一条规则，不受影响
        let report = KnowledgeExtractor::builtin()
            .extract("sk_live_aaaaaaaaaaaaaaaaaaaa1", &[])
            .unwrap();
        let names: Vec<_> = report.findings.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(names, ["stripe_secret"]);
    }

    #[test]
    fn openai_key_ignores_css_variables_and_slugs() {
        for body in [
            "<style>:root{--sk-line-height--extralarge:1.5}</style>",
            "<i class='sk-color-icon-secondary'></i>",
            "input::placeholder{color:var(--sk-input-color-placeholder)}",
            "<a href='/news/sk-hynix-reports-record-profit'>SK</a>",
            // 新格式前缀但没有数字
            "sk-proj-abcdefghijklmnopqrstuvwxyz",
            // 旧格式长度够但全是字母
            "sk-abcdefghijklmnopqrstuvwxyzabcdefgh",
        ] {
            assert_eq!(openai_hits(body), 0, "{body}");
        }
    }
}
