//! Excel（SpreadsheetML，ECMA-376 第 1 部分第 18 章）转 Markdown：每个工作表一个二级标题和一张表。
//!
//! 单元格输出显示值：数值按单元格样式的数字格式渲染（[`numfmt`]），共享字符串、行内字符串、
//! 布尔、错误值照原样。表的范围是有内容的单元格的外接矩形，第一行当表头；合并区域只有左上角有值。
//! 公式只取缓存的结果，不重新计算。图表、图片、批注不输出。

mod datefmt;
mod numfmt;
mod sheet;

use std::collections::HashMap;

use crate::md::{Block, escape, render};
use crate::opc::{self, Rels};
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Options, check_deadline};

pub(crate) struct Book {
    shared: Vec<String>,
    /// cellXfs 下标 → 格式代码。
    formats: Vec<String>,
    date1904: bool,
}

impl Book {
    /// 一个数值单元格的显示值。
    fn number(&self, raw: &str, style: usize) -> String {
        let Ok(v) = raw.trim().parse::<f64>() else {
            return raw.to_string();
        };
        let code = self.formats.get(style).map_or("General", String::as_str);
        numfmt::format(v, code, self.date1904)
    }
}

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    let main = opc::main_part(zip, "xl/workbook.xml")?;
    let xml = zip
        .read_text(&main)?
        .ok_or_else(|| Error::Malformed("no xl/workbook.xml".into()))?;
    let rels = Rels::of(zip, &main)?;
    let (sheets, date1904) = workbook(&xml)?;
    let shared = match rels.first("sharedStrings") {
        Some(r) => zip
            .read_text(&r.target)?
            .map(|x| shared_strings(&x))
            .transpose()?
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let formats = match rels.first("styles") {
        Some(r) => zip
            .read_text(&r.target)?
            .map(|x| cell_formats(&x))
            .transpose()?
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let book = Book {
        shared,
        formats,
        date1904,
    };

    let mut out = Converted::default();
    let mut blocks = Vec::new();
    let mut budget = opts.limits.max_cells;
    for (name, rid) in &sheets {
        check_deadline(opts)?;
        blocks.push(Block::Heading(2, escape(name)));
        let Some(rel) = rels.get(rid).filter(|r| r.kind == "worksheet") else {
            continue; // 图表工作表、对话框工作表没有单元格
        };
        let Some(sx) = zip.read_text(&rel.target)? else {
            continue;
        };
        let grid = sheet::read(&sx, &book, opts, &mut budget, &mut out.warnings)?;
        if !grid.is_empty() {
            blocks.push(Block::Table(grid));
        }
    }
    out.parts = sheets.len();
    out.markdown = render(&blocks);
    opc::core_properties(zip, &mut out)?;
    Ok(out)
}

/// 工作表（名字、关系 id，按工作簿里的顺序）和日期系统。
fn workbook(xml: &str) -> Result<(Vec<(String, String)>, bool), Error> {
    let mut sheets = Vec::new();
    let mut date1904 = false;
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        let Tok::Start(name, a) = t else { continue };
        match local_name(name) {
            "workbookPr" => date1904 = a.get("date1904").is_some_and(|v| v == "1" || v == "true"),
            "sheet" => {
                if let (Some(n), Some(id)) = (a.get("name"), a.get("id")) {
                    sheets.push((n.into_owned(), id.into_owned()));
                }
            }
            _ => {}
        }
    }
    Ok((sheets, date1904))
}

/// sharedStrings.xml：每个 `si` 的文字（富文本各段拼起来，注音 `rPh` 不要）。
fn shared_strings(xml: &str) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        let Tok::Start(name, _) = t else { continue };
        if local_name(name) != "si" {
            continue;
        }
        let mut s = String::new();
        let mut depth = 1;
        while let Some(t) = r.next()? {
            match t {
                Tok::Start(n, _) => match local_name(n) {
                    "t" => s.push_str(&r.text_until_end()?),
                    "rPh" | "phoneticPr" => r.skip()?,
                    _ => depth += 1,
                },
                Tok::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Text(_) => {}
            }
        }
        out.push(s);
    }
    Ok(out)
}

/// styles.xml：每个单元格格式（cellXfs/xf）对应的格式代码。
fn cell_formats(xml: &str) -> Result<Vec<String>, Error> {
    let mut custom: HashMap<u32, String> = HashMap::new();
    let mut ids = Vec::new();
    let mut in_xfs = false;
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (
                        a.get("numFmtId").and_then(|v| v.parse().ok()),
                        a.get("formatCode"),
                    ) {
                        custom.insert(id, code.into_owned());
                    }
                }
                "cellXfs" => in_xfs = true,
                "xf" if in_xfs => {
                    ids.push(
                        a.get("numFmtId")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0),
                    );
                    r.skip()?;
                }
                _ => {}
            },
            Tok::End(name) if local_name(name) == "cellXfs" => in_xfs = false,
            _ => {}
        }
    }
    Ok(ids
        .into_iter()
        .map(|id| {
            custom
                .get(&id)
                .cloned()
                .or_else(|| numfmt::builtin(id).map(str::to_string))
                .unwrap_or_else(|| "General".into())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::build;

    fn xlsx(sheets: &[(&str, &str)], shared: &str, styles: &str) -> Vec<u8> {
        let mut wb = String::from(r#"<workbook xmlns:r="r"><workbookPr/><sheets>"#);
        let mut rels = String::from("<Relationships>");
        let mut parts: Vec<(String, String)> = Vec::new();
        for (i, (name, data)) in sheets.iter().enumerate() {
            wb += &format!(r#"<sheet name="{name}" sheetId="{i}" r:id="s{i}"/>"#);
            rels += &format!(
                r#"<Relationship Id="s{i}" Type="x/worksheet" Target="worksheets/sheet{i}.xml"/>"#
            );
            parts.push((
                format!("xl/worksheets/sheet{i}.xml"),
                format!("<worksheet><sheetData>{data}</sheetData></worksheet>"),
            ));
        }
        wb += "</sheets></workbook>";
        rels += r#"<Relationship Id="ss" Type="x/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="st" Type="x/styles" Target="styles.xml"/></Relationships>"#;
        parts.push(("xl/workbook.xml".into(), wb));
        parts.push(("xl/_rels/workbook.xml.rels".into(), rels));
        parts.push((
            "xl/sharedStrings.xml".into(),
            format!("<sst>{shared}</sst>"),
        ));
        parts.push(("xl/styles.xml".into(), styles.into()));
        let refs: Vec<(&str, &[u8], bool)> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_bytes(), true))
            .collect();
        build(&refs)
    }

    #[test]
    fn sheets_become_headed_tables_with_display_values() {
        let shared = "<si><t>Name</t></si><si><r><t>Da</t></r><r><t>te</t></r><rPh><t>x</t></rPh></si><si><t>Bob | Jr</t></si>";
        let styles = r#"<styleSheet><numFmts><numFmt numFmtId="164" formatCode="0.0%"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;
        let s1 = r#"<row r="2"><c r="B2" t="s"><v>0</v></c><c r="C2" t="s"><v>1</v></c><c r="D2"><v>3</v></c></row>
<row r="3"><c r="B3" t="s"><v>2</v></c><c r="C3" s="1"><v>37653</v></c><c r="D3" s="2"><v>0.279</v></c></row>
<row r="4"><c r="B4" t="b"><v>1</v></c><c r="C4" t="e"><v>#N/A</v></c><c r="D4" t="inlineStr"><is><t>inline</t></is></c></row>"#;
        let bytes = xlsx(&[("Data", s1), ("Empty", "")], shared, styles);
        let got = crate::convert(&bytes, crate::Format::Xlsx, &Options::default()).unwrap();
        assert_eq!(
            got.markdown,
            "## Data\n\n| Name | Date | 3 |\n| --- | --- | --- |\n| Bob \\| Jr | 2/1/2003 | 27.9% |\n| TRUE | #N/A | inline |\n\n## Empty\n"
        );
        assert_eq!(got.parts, 2);
    }

    #[test]
    fn hidden_rows_and_columns_are_left_out() {
        // 解析器不管 cols 写在哪一层，测试里直接放进 sheetData
        let s = r#"<cols><col min="2" max="2" hidden="1"/></cols>
<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>
<row r="2" hidden="1"><c r="A2"><v>4</v></c></row><row r="3"><c r="A3"><v>5</v></c></row>"#;
        let bytes = xlsx(&[("S", s)], "", "<styleSheet/>");
        let got = crate::convert(&bytes, crate::Format::Xlsx, &Options::default()).unwrap();
        assert_eq!(got.markdown, "## S\n\n| 1 | 3 |\n| --- | --- |\n| 5 |  |\n");
    }

    #[test]
    fn huge_and_sparse_sheets_are_capped() {
        // 一个单元格放在 XFD1048576：范围巨大但只有两个值，按上限截掉并警告，不分配整张表
        let s = r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="1048576"><c r="XFD1048576"><v>2</v></c></row>"#;
        let bytes = xlsx(&[("Wide", s)], "", "<styleSheet/>");
        let got = crate::convert(&bytes, crate::Format::Xlsx, &Options::default()).unwrap();
        assert!(got.markdown.len() < 10_000, "{}", got.markdown.len());
        assert!(!got.warnings.is_empty());
        let rows: String = (1..=50)
            .map(|i| {
                format!(
                    r#"<row r="{i}"><c r="A{i}"><v>{i}</v></c><c r="B{i}"><v>{i}</v></c></row>"#
                )
            })
            .collect();
        let bytes = xlsx(&[("Long", &rows)], "", "<styleSheet/>");
        let opts = Options {
            limits: crate::Limits {
                max_cells: 20,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = crate::convert(&bytes, crate::Format::Xlsx, &opts).unwrap();
        assert_eq!(
            got.markdown.lines().filter(|l| l.starts_with("| ")).count(),
            11
        );
        assert!(
            got.warnings.iter().any(|w| w.contains("cell limit")),
            "{:?}",
            got.warnings
        );
    }
}
