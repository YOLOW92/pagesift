//! 图表（DrawingML Chart，ECMA-376 21.2）：标题和缓存的数据（系列 × 分类）变成一张表。
//! 只读文件里缓存的值，不去外部工作簿取数。

use crate::Error;
use crate::md::{escape, squeeze};
use crate::xml::{Reader, Tok, local_name};

#[derive(Default)]
struct Series {
    name: String,
    cats: Vec<(usize, String)>,
    vals: Vec<(usize, String)>,
}

/// 返回（标题，表格）。没有数据时表格为空。
pub(super) fn read(xml: &str) -> Result<(String, Vec<Vec<String>>), Error> {
    let mut title = String::new();
    let mut series: Vec<Series> = Vec::new();
    let mut r = Reader::new(xml);
    // 当前在 c:ser 里的哪一部分：tx / cat / val
    let mut part = "";
    let mut in_title = false;
    let mut idx = 0usize;
    let mut stack: Vec<&str> = Vec::new();
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => {
                let local = local_name(name);
                match local {
                    "title" if !stack.contains(&"ser") && stack.last() == Some(&"chart") => {
                        in_title = true
                    }
                    "ser" => series.push(Series::default()),
                    "tx" if stack.last() == Some(&"ser") => part = "tx",
                    "cat" | "xVal" => part = "cat",
                    "val" | "yVal" => part = "val",
                    "pt" => idx = a.get("idx").and_then(|v| v.parse().ok()).unwrap_or(0),
                    "t" if in_title => {
                        title.push_str(&r.text_until_end()?);
                        continue;
                    }
                    "v" => {
                        let v = r.text_until_end()?;
                        if let Some(s) = series.last_mut() {
                            match part {
                                "tx" => s.name = v,
                                "cat" => s.cats.push((idx, v)),
                                "val" => s.vals.push((idx, v)),
                                _ => {}
                            }
                        }
                        continue;
                    }
                    "extLst" | "spPr" | "txPr" => {
                        r.skip()?;
                        continue;
                    }
                    _ => {}
                }
                stack.push(local);
            }
            Tok::End(name) => {
                let local = local_name(name);
                stack.pop();
                match local {
                    "title" => in_title = false,
                    "tx" | "cat" | "val" | "xVal" | "yVal" => part = "",
                    _ => {}
                }
            }
            Tok::Text(_) => {}
        }
    }
    let n = series
        .iter()
        .flat_map(|s| s.cats.iter().chain(&s.vals))
        .map(|(i, _)| i + 1)
        .max()
        .unwrap_or(0)
        .min(10_000);
    if series.is_empty() || n == 0 {
        return Ok((squeeze(&title), Vec::new()));
    }
    let mut header = vec![String::new()];
    header.extend(series.iter().enumerate().map(|(i, s)| {
        if s.name.is_empty() {
            format!("Series {}", i + 1)
        } else {
            escape(&s.name)
        }
    }));
    let mut rows = vec![header];
    let cats = &series[0].cats;
    for i in 0..n {
        let mut row = vec![
            cats.iter()
                .find(|c| c.0 == i)
                .map(|c| escape(&c.1))
                .unwrap_or_else(|| (i + 1).to_string()),
        ];
        row.extend(series.iter().map(|s| {
            s.vals
                .iter()
                .find(|v| v.0 == i)
                .map(|v| escape(&v.1))
                .unwrap_or_default()
        }));
        rows.push(row);
    }
    Ok((squeeze(&title), rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cached_series() {
        let xml = r#"<c:chartSpace><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title>
<c:plotArea><c:barChart><c:ser><c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>2025</c:v></c:pt></c:strCache></c:strRef></c:tx>
<c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:strCache></c:strRef></c:cat>
<c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>4.3</c:v></c:pt><c:pt idx="1"><c:v>2.5</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let (title, rows) = read(xml).unwrap();
        assert_eq!(title, "Sales");
        assert_eq!(
            rows,
            vec![vec!["", "2025"], vec!["Q1", "4.3"], vec!["Q2", "2.5"]]
        );
    }
}
