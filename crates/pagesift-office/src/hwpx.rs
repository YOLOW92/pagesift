//! 韩文 HWPX（KS X 6101）：ZIP 里 `Contents/section*.xml` 是正文，`hp:p`、`hp:t`、`hp:tbl`。
//! 标题在 `Contents/content.hpf`。样式不要。

use crate::md::{self, Block};
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Options, budget, check_deadline};

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    if let Some(manifest) = zip.read_text("META-INF/manifest.xml")?
        && manifest.contains("encryption-data")
    {
        return Err(Error::Encrypted);
    }
    let mut names: Vec<(u32, String)> = zip
        .names()
        .filter_map(|(n, _)| {
            let num = n.strip_prefix("Contents/section")?.strip_suffix(".xml")?;
            Some((num.parse().ok()?, n.to_owned()))
        })
        .collect();
    if names.is_empty() {
        return Err(Error::Malformed("no Contents/section0.xml".into()));
    }
    names.sort();
    let mut blocks = Vec::new();
    for (_, name) in &names {
        check_deadline(opts)?;
        if let Some(xml) = zip.read_text(name)? {
            section(&xml, &mut blocks, opts)?;
        }
        if budget::exhausted() {
            break;
        }
    }
    let title = match zip.read_text("Contents/content.hpf")? {
        Some(hpf) => read_title(&hpf)?,
        None => None,
    };
    Ok(Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        title,
        parts: names.len(),
        ..Default::default()
    })
}

fn read_title(hpf: &str) -> Result<Option<String>, Error> {
    let mut r = Reader::new(hpf);
    while let Some(tok) = r.next()? {
        if let Tok::Start(name, _) = tok
            && local_name(name) == "title"
        {
            let t = r.text_until_end()?;
            return Ok(Some(t.trim().to_owned()).filter(|t| !t.is_empty()));
        }
    }
    Ok(None)
}

fn section(xml: &str, blocks: &mut Vec<Block>, opts: &Options<'_>) -> Result<(), Error> {
    let mut r = Reader::new(xml);
    let mut tables: Vec<Table> = Vec::new();
    let mut para = String::new();
    let mut in_text = 0usize;
    let mut steps = 0usize;
    while let Some(tok) = r.next()? {
        steps += 1;
        if steps.is_multiple_of(256) {
            check_deadline(opts)?;
        }
        match tok {
            Tok::Start(name, _) => match local_name(name) {
                "t" => in_text += 1,
                "tab" if in_text > 0 => para.push('\t'),
                "lineBreak" if in_text > 0 => para.push('\n'),
                "nbSpace" | "fwSpace" | "hyphen" if in_text > 0 => para.push(' '),
                "tbl" => {
                    flush(&mut para, &mut tables, blocks);
                    tables.push(Table::default());
                }
                _ => {}
            },
            Tok::End(name) => match local_name(name) {
                "t" => in_text = in_text.saturating_sub(1),
                "p" => flush(&mut para, &mut tables, blocks),
                "tc" => {
                    flush(&mut para, &mut tables, blocks);
                    if let Some(t) = tables.last_mut() {
                        let cell = std::mem::take(&mut t.cell);
                        t.row.push(md::escape(&md::squeeze(&cell)));
                    }
                }
                "tr" => {
                    if let Some(t) = tables.last_mut() {
                        let row = std::mem::take(&mut t.row);
                        t.rows.push(row);
                    }
                }
                "tbl" => {
                    let Some(t) = tables.pop() else { continue };
                    match tables.last_mut() {
                        // 嵌套表格压平到外层单元格
                        Some(outer) => {
                            for row in &t.rows {
                                outer.cell.push_str(&row.join(" "));
                                outer.cell.push(' ');
                            }
                        }
                        None if !t.rows.is_empty() => {
                            let mut rows = t.rows;
                            let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                            for row in &mut rows {
                                row.resize(width, String::new());
                            }
                            blocks.push(Block::Table(rows));
                        }
                        None => {}
                    }
                }
                _ => {}
            },
            Tok::Text(t) if in_text > 0 => {
                budget::spend(t.len());
                para.push_str(&t);
            }
            Tok::Text(_) => {}
        }
    }
    flush(&mut para, &mut tables, blocks);
    Ok(())
}

fn flush(para: &mut String, tables: &mut [Table], blocks: &mut Vec<Block>) {
    let text = std::mem::take(para);
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match tables.last_mut() {
        Some(t) => {
            if !t.cell.is_empty() {
                t.cell.push(' ');
            }
            t.cell.push_str(text);
        }
        None => blocks.push(Block::Para(md::escape(text))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;

    #[test]
    fn paragraphs_tables_and_title() {
        let sec = "<hs:sec xmlns:hs='a' xmlns:hp='b'>\
            <hp:p><hp:run><hp:t>\u{d55c}\u{ae00}<hp:tab/>x</hp:t></hp:run></hp:p>\
            <hp:p><hp:run><hp:tbl><hp:tr>\
              <hp:tc><hp:subList><hp:p><hp:run><hp:t>a1</hp:t></hp:run></hp:p></hp:subList></hp:tc>\
              <hp:tc><hp:subList><hp:p><hp:run><hp:t>b1</hp:t></hp:run></hp:p></hp:subList></hp:tc>\
            </hp:tr></hp:tbl></hp:run></hp:p>\
            <hp:p><hp:run><hp:t>end</hp:t></hp:run></hp:p></hs:sec>";
        let hpf = "<opf:package xmlns:opf='o'><opf:metadata><opf:title>T</opf:title></opf:metadata></opf:package>";
        let bytes = crate::zip::tests::build(&[
            ("mimetype", b"application/hwp+zip", false),
            ("Contents/section0.xml", sec.as_bytes(), false),
            ("Contents/content.hpf", hpf.as_bytes(), false),
        ]);
        let zip = Zip::open(&bytes, &Limits::default()).unwrap();
        let d = convert(&zip, &Options::default()).unwrap();
        assert_eq!(d.title.as_deref(), Some("T"));
        assert!(d.markdown.starts_with("\u{d55c}\u{ae00}"), "{}", d.markdown);
        assert!(d.markdown.contains("| a1 | b1 |"), "{}", d.markdown);
        assert!(d.markdown.trim_end().ends_with("end"), "{}", d.markdown);
    }

    #[test]
    fn missing_sections_are_an_error() {
        let bytes = crate::zip::tests::build(&[("mimetype", b"application/hwp+zip", false)]);
        let zip = Zip::open(&bytes, &Limits::default()).unwrap();
        assert!(matches!(
            convert(&zip, &Options::default()),
            Err(Error::Malformed(_))
        ));
    }
}
