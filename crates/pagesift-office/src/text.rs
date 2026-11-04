//! 文本家族：纯文本和轻量标记原样输出，结构化文本放进代码块，Markdown 取标题后原样输出。
//! 这些格式本身就是给人读的，转换只做换行统一和预算控制，不重新排版。

use std::borrow::Cow;

use crate::{Converted, Error, Format, Options, budget, check_deadline};

pub(crate) fn convert(text: &str, format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    check_deadline(opts)?;
    let text = normalize(text);
    let title = match format {
        Format::Markdown => markdown_title(&text),
        _ => None,
    };
    let mut parts = text.lines().count();
    let mut body = text.into_owned();
    let whole = budget::clip(&mut body);
    let mut metadata = Vec::new();
    let markdown = match format {
        Format::Fenced(lang) => {
            let mut out = String::with_capacity(body.len() + 16);
            // 被预算截断的代码不解析：半截文件的结构不可信
            let outline = if whole {
                pagesift_syntax::outline(lang, &body)
            } else {
                None
            };
            match outline {
                Some(o) => {
                    parts = o.symbols.len();
                    sections_into(&mut out, lang, &body, &o);
                    metadata = code_metadata(lang, &o);
                }
                None => fence_into(&mut out, lang, &body),
            }
            out
        }
        _ => {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body
        }
    };
    Ok(Converted {
        markdown,
        title,
        metadata,
        parts,
        ..Default::default()
    })
}

/// 代码文件按顶层函数和类分段：每段一个标题加一个代码块，切块时可以按标题切。
fn sections_into(out: &mut String, lang: &str, body: &str, outline: &pagesift_syntax::Outline) {
    for section in &outline.sections {
        let text = body.get(section.start..section.end).unwrap_or("");
        let text = text.trim_start_matches('\n').trim_end();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        if let Some(title) = &section.title {
            let clean = title.replace('`', "");
            match clean.split_once(' ') {
                Some((label, name)) => {
                    out.push_str(&format!("## {label} `{name}`\n\n"));
                }
                None => out.push_str(&format!("## {clean}\n\n")),
            }
        }
        fence_into(out, lang, text);
    }
}

/// 元数据里每种列表最多放这么多项，超出的写总数。
const MAX_LISTED: usize = 200;

fn code_metadata(lang: &str, outline: &pagesift_syntax::Outline) -> Vec<(String, String)> {
    use pagesift_syntax::SymbolKind as K;
    let names = |want: &dyn Fn(K) -> bool| -> Vec<&str> {
        let is_named = |s: &&pagesift_syntax::Symbol| want(s.kind) && !s.name.is_empty();
        outline
            .symbols
            .iter()
            .filter(is_named)
            .map(|s| s.name.as_str())
            .collect()
    };
    let functions = names(&|k| matches!(k, K::Function | K::Method));
    let classes = names(&|k| {
        matches!(
            k,
            K::Class | K::Struct | K::Enum | K::Interface | K::Trait | K::Type
        )
    });
    let imports: Vec<&str> = outline.imports.iter().map(String::as_str).collect();
    let mut out = vec![("language".to_string(), lang.to_string())];
    for (key, list) in [
        ("functions", functions),
        ("classes", classes),
        ("imports", imports),
    ] {
        if !list.is_empty() {
            let mut value = list[..list.len().min(MAX_LISTED)].join(", ");
            if list.len() > MAX_LISTED {
                value.push_str(&format!(" (+{} more)", list.len() - MAX_LISTED));
            }
            out.push((key.to_string(), value));
        }
    }
    out
}

/// 去掉 BOM，换行统一成 `\n`。
fn normalize(text: &str) -> Cow<'_, str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// 写一个代码块。围栏比内容里最长的反引号串多一个（CommonMark 的规则），内容里有 ``` 也不会提前结束。
pub(crate) fn fence_into(out: &mut String, lang: &str, body: &str) {
    let mut longest = 0;
    let mut run = 0;
    for b in body.bytes() {
        run = if b == b'`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    out.push_str(&fence);
    out.push_str(lang);
    out.push('\n');
    out.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
}

/// Markdown 的标题：front matter 的 `title:`，否则代码块之外第一个一级标题。
fn markdown_title(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        for line in rest[..end].lines() {
            if let Some(v) = line.strip_prefix("title:") {
                let v = v.trim().trim_matches(|c| c == '"' || c == '\'').trim();
                if !v.is_empty() {
                    return Some(v.to_owned());
                }
            }
        }
    }
    let mut in_code = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
        } else if !in_code && let Some(h) = t.strip_prefix("# ") {
            let h = h.trim().trim_end_matches('#').trim();
            if !h.is_empty() {
                return Some(h.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, format: Format) -> Converted {
        convert(text, format, &Options::default()).unwrap()
    }

    #[test]
    fn plain_text_stays_as_is_with_unified_newlines() {
        let d = run("\u{feff}a *b*\r\nc\rd", Format::Text);
        assert_eq!(d.markdown, "a *b*\nc\nd\n");
        assert_eq!(d.parts, 3);
        assert_eq!(run("", Format::Text).markdown, "");
    }

    #[test]
    fn structured_text_goes_into_a_fence_longer_than_its_backticks() {
        assert_eq!(
            run("a: 1", Format::Fenced("yaml")).markdown,
            "```yaml\na: 1\n```\n"
        );
        assert_eq!(
            run("x ```` y\n", Format::Fenced("toml")).markdown,
            "`````toml\nx ```` y\n`````\n"
        );
    }

    #[test]
    fn markdown_title_comes_from_front_matter_then_first_heading() {
        let t = |s| run(s, Format::Markdown).title;
        assert_eq!(t("---\ntitle: \"Hi\"\n---\n# Other"), Some("Hi".into()));
        assert_eq!(t("intro\n\n# Head #\n"), Some("Head".into()));
        assert_eq!(t("```\n# not\n```\n## two"), None);
        let d = run("# A\n\nbody", Format::Markdown);
        assert_eq!(d.markdown, "# A\n\nbody\n");
    }

    #[test]
    fn python_is_split_by_function_and_class() {
        let out = run(
            "import os\n\n# says hi\ndef hi():\n    return 1\n\nclass A:\n    def m(self):\n        pass\n",
            Format::Fenced("python"),
        );
        assert_eq!(
            out.markdown,
            "```python\nimport os\n```\n\n## function `hi`\n\n```python\n# says hi\ndef hi():\n    return 1\n```\n\n## class `A`\n\n```python\nclass A:\n    def m(self):\n        pass\n```\n"
        );
        let get = |k: &str| {
            out.metadata
                .iter()
                .find(|(a, _)| a == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("language"), Some("python"));
        assert_eq!(get("functions"), Some("hi, m"));
        assert_eq!(get("classes"), Some("A"));
        assert_eq!(get("imports"), Some("import os"));
    }

    #[test]
    fn code_without_symbols_stays_one_block() {
        let out = run("x = 1\n", Format::Fenced("python"));
        assert_eq!(out.markdown, "```python\nx = 1\n```\n");
    }
}
