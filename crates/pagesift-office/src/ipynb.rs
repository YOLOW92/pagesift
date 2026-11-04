//! Jupyter 笔记本（nbformat 3、4）：Markdown 单元原样，代码单元进代码块（语言取自 kernelspec），
//! 文字输出进 `text` 代码块，错误写成 `名字: 说明`。图片输出不进正文，只计数给警告。

use serde_json::Value;

use crate::text::fence_into;
use crate::{Converted, Error, Options, budget, check_deadline};

pub(crate) fn convert(text: &str, opts: &Options<'_>) -> Result<Converted, Error> {
    let nb: Value = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| Error::Malformed(format!("notebook is not JSON: {e}")))?;
    let lang = [
        "/metadata/kernelspec/language",
        "/metadata/language_info/name",
    ]
    .iter()
    .find_map(|p| nb.pointer(p).and_then(Value::as_str))
    .unwrap_or("");
    let cells = nb
        .get("cells")
        .or_else(|| nb.pointer("/worksheets/0/cells"))
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Malformed("notebook has no cells".into()))?;
    let mut out = String::new();
    let mut title = None;
    let mut images = 0usize;
    for cell in cells {
        check_deadline(opts)?;
        let before = out.len();
        match cell.get("cell_type").and_then(Value::as_str).unwrap_or("") {
            "markdown" | "heading" | "raw" => {
                let source = joined(cell.get("source"));
                if title.is_none() {
                    title = first_heading(&source);
                }
                push_block(&mut out, &source);
            }
            "code" => {
                let source = joined(cell.get("source").or_else(|| cell.get("input")));
                if !source.trim().is_empty() {
                    separate(&mut out);
                    fence_into(&mut out, lang, &source);
                }
                for output in cell
                    .get("outputs")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                {
                    let body = output_text(output, &mut images);
                    if !body.trim().is_empty() {
                        separate(&mut out);
                        fence_into(&mut out, "text", &body);
                    }
                }
            }
            _ => {}
        }
        if !budget::spend(out.len() - before) {
            break;
        }
    }
    let mut warnings = Vec::new();
    if images > 0 {
        warnings.push(format!(
            "notebook_images_skipped: {images} image outputs were left out"
        ));
    }
    Ok(Converted {
        markdown: out,
        title,
        warnings,
        parts: cells.len(),
        ..Default::default()
    })
}

fn separate(out: &mut String) {
    if !out.is_empty() {
        out.push('\n');
    }
}

fn push_block(out: &mut String, source: &str) {
    let source = source.trim_end();
    if source.is_empty() {
        return;
    }
    separate(out);
    out.push_str(source);
    out.push('\n');
}

/// nbformat 里多行文字既可以是字符串，也可以是字符串数组。
fn joined(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn output_text(output: &Value, images: &mut usize) -> String {
    match output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "stream" => joined(output.get("text")),
        "error" | "pyerr" => {
            let name = output
                .get("ename")
                .and_then(Value::as_str)
                .unwrap_or("Error");
            let value = output.get("evalue").and_then(Value::as_str).unwrap_or("");
            format!("{name}: {value}")
        }
        // nbformat 4 放在 data 里；nbformat 3 直接放在输出上
        _ => {
            let data = output.get("data").unwrap_or(output);
            if [
                "image/png",
                "image/jpeg",
                "image/svg+xml",
                "png",
                "jpeg",
                "svg",
            ]
            .iter()
            .any(|k| data.get(*k).is_some())
            {
                *images += 1;
            }
            joined(data.get("text/plain").or_else(|| data.get("text")))
        }
    }
}

fn first_heading(source: &str) -> Option<String> {
    source
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("# "))
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> Converted {
        convert(s, &Options::default()).unwrap()
    }

    #[test]
    fn cells_and_outputs_in_order() {
        let nb = r##"{"metadata":{"kernelspec":{"language":"python"}},"cells":[
          {"cell_type":"markdown","source":["# T\n","intro"]},
          {"cell_type":"code","source":"print(1)","outputs":[
            {"output_type":"stream","text":["1\n"]},
            {"output_type":"execute_result","data":{"text/plain":"2","image/png":"x"}},
            {"output_type":"error","ename":"E","evalue":"bad","traceback":["\u001b[0m"]}
          ]}]}"##;
        let d = run(nb);
        assert_eq!(d.title.as_deref(), Some("T"));
        assert_eq!(d.parts, 2);
        assert_eq!(
            d.markdown,
            "# T\nintro\n\n```python\nprint(1)\n```\n\n```text\n1\n```\n\n```text\n2\n```\n\n```text\nE: bad\n```\n"
        );
        assert!(d.warnings[0].starts_with("notebook_images_skipped: 1"));
    }

    #[test]
    fn nbformat_3_and_broken_input() {
        let v3 = r#"{"worksheets":[{"cells":[{"cell_type":"code","input":"x","outputs":[{"output_type":"pyout","text":"y"}]}]}]}"#;
        assert_eq!(run(v3).markdown, "```\nx\n```\n\n```text\ny\n```\n");
        for bad in ["not json", "{}", r#"{"cells":1}"#] {
            assert!(matches!(
                convert(bad, &Options::default()),
                Err(Error::Malformed(_))
            ));
        }
    }
}
