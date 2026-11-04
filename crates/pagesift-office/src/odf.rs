//! OpenDocument（ISO/IEC 26300）：ODT、ODS、ODP。正文都在 content.xml，标题在 meta.xml。
//! 三种共用一个遍历：标题、段落、列表、表格；ODS 的每张表前写表名，ODP 的每页前写页码注释。
//! 重复行列（`number-rows-repeated` 可以写到一百万）有上限，空的不展开。

use crate::md::{self, Block};
use crate::xml::{Attrs, Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Format, Options, check_deadline};

/// 单个非空单元格或行最多展开这么多次。
const MAX_REPEAT: usize = 1_024;

pub(crate) fn convert(
    zip: &Zip<'_>,
    format: Format,
    opts: &Options<'_>,
) -> Result<Converted, Error> {
    if let Some(manifest) = zip.read_text("META-INF/manifest.xml")?
        && manifest.contains("encryption-data")
    {
        return Err(Error::Encrypted);
    }
    let content = zip
        .read_text("content.xml")?
        .ok_or_else(|| Error::Malformed("content.xml is missing".into()))?;
    let mut out = walk_content(&content, format, opts)?;
    out.title = match zip.read_text("meta.xml")? {
        Some(meta) => read_title(&meta)?,
        None => None,
    };
    Ok(out)
}

fn walk_content(content: &str, format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    let mut walk = Walk {
        format,
        opts,
        blocks: Vec::new(),
        lists: 0,
        slides: 0,
        tables: 0,
        cells: 0,
        warnings: Vec::new(),
    };
    walk.run(&mut Reader::new(content))?;
    let parts = match format {
        Format::Ods => walk.tables,
        Format::Odp => walk.slides,
        _ => walk.blocks.len(),
    };
    Ok(Converted {
        markdown: if walk.blocks.is_empty() {
            String::new()
        } else {
            md::render(&walk.blocks)
        },
        warnings: walk.warnings,
        parts,
        ..Default::default()
    })
}

/// 单文件 XML 的 OpenDocument：根元素写明类型，正文和元数据都在同一个文件里。
pub(crate) fn convert_flat(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let content = crate::xml::decode(bytes.to_vec());
    let mut head = Reader::new(&content);
    let mut format = None;
    let mut title = None;
    while let Some(tok) = head.next()? {
        let Tok::Start(name, attrs) = tok else {
            continue;
        };
        match local_name(name) {
            "document" => {
                let mime = attrs.get("mimetype").unwrap_or_default();
                format = Some(if mime.contains("spreadsheet") {
                    Format::Ods
                } else if mime.contains("presentation") || mime.contains("graphics") {
                    Format::Odp
                } else {
                    Format::Odt
                });
            }
            "title" => {
                let t = head.text_until_end()?;
                title = Some(t.trim().to_owned()).filter(|t| !t.is_empty());
                break;
            }
            // 元数据段没有标题就不再往后找：正文里的 title 是别的东西
            "body" | "styles" | "automatic-styles" | "master-styles" => break,
            _ => {}
        }
    }
    let format = format.ok_or_else(|| Error::Malformed("not an OpenDocument XML file".into()))?;
    let mut out = walk_content(&content, format, opts)?;
    out.title = title;
    Ok(out)
}

fn read_title(meta: &str) -> Result<Option<String>, Error> {
    let mut r = Reader::new(meta);
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

struct Walk<'o, 'a> {
    format: Format,
    opts: &'o Options<'a>,
    blocks: Vec<Block>,
    lists: usize,
    slides: usize,
    tables: usize,
    cells: usize,
    warnings: Vec<String>,
}

impl Walk<'_, '_> {
    fn run(&mut self, r: &mut Reader<'_>) -> Result<(), Error> {
        let mut steps = 0usize;
        while let Some(tok) = r.next()? {
            steps += 1;
            if steps.is_multiple_of(256) {
                check_deadline(self.opts)?;
            }
            match tok {
                Tok::Start(name, attrs) => match local_name(name) {
                    "automatic-styles" | "styles" | "master-styles" | "meta"
                    | "font-face-decls" | "scripts" | "forms" | "tracked-changes"
                    | "annotation" | "notes" | "settings" => r.skip()?,
                    "h" => {
                        let level = attrs
                            .get("outline-level")
                            .and_then(|l| l.parse::<u8>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6);
                        let text = inline(r)?;
                        if !text.is_empty() {
                            self.blocks.push(Block::Heading(level, md::escape(&text)));
                        }
                    }
                    "p" => {
                        let text = inline(r)?;
                        if text.is_empty() {
                            continue;
                        }
                        let text = md::escape(&text);
                        self.blocks.push(if self.lists > 0 {
                            Block::Item {
                                level: u8::try_from(self.lists - 1).unwrap_or(u8::MAX),
                                ordered: false,
                                number: 0,
                                text,
                            }
                        } else {
                            Block::Para(text)
                        });
                    }
                    "list" => self.lists += 1,
                    "page" if self.format == Format::Odp => {
                        self.slides += 1;
                        self.blocks
                            .push(Block::Raw(format!("<!-- Slide {} -->", self.slides)));
                    }
                    "table" => self.table(r, &attrs)?,
                    _ => {}
                },
                Tok::End(name) if local_name(name) == "list" => {
                    self.lists = self.lists.saturating_sub(1);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn table(&mut self, r: &mut Reader<'_>, attrs: &Attrs<'_>) -> Result<(), Error> {
        self.tables += 1;
        if self.format == Format::Ods
            && let Some(name) = attrs.get("name")
        {
            self.blocks.push(Block::Heading(2, md::escape(&name)));
        }
        let max_columns = self.opts.limits.max_columns;
        let max_cells = self.opts.limits.max_cells;
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut row: Vec<String> = Vec::new();
        let mut row_repeat = 1usize;
        let mut depth = 1usize;
        let mut truncated = false;
        while depth > 0 {
            match r.next()? {
                Some(Tok::Start(name, a)) => match local_name(name) {
                    "table-row" => {
                        row_repeat = repeat(&a, "number-rows-repeated");
                        depth += 1;
                    }
                    "table-cell" => {
                        let n = repeat(&a, "number-columns-repeated");
                        let text = md::escape(inline(r)?.trim());
                        // 表尾铺满整行的空单元格写一次就够了，真正的空列留到下面裁掉
                        let n = if text.is_empty() { n.min(1) } else { n };
                        for _ in 0..n {
                            if row.len() < max_columns {
                                row.push(text.clone());
                            }
                        }
                    }
                    "covered-table-cell" => {
                        row.push(String::new());
                        r.skip()?;
                    }
                    "table-source" | "shapes" => r.skip()?,
                    _ => depth += 1,
                },
                Some(Tok::End(name)) => {
                    depth -= 1;
                    if local_name(name) == "table-row" {
                        while row.last().is_some_and(String::is_empty) {
                            row.pop();
                        }
                        let cells = row.len();
                        let finished = std::mem::take(&mut row);
                        // 空行不展开：工作表末尾常有上百万空行
                        if cells > 0 {
                            for _ in 0..row_repeat {
                                if self.cells + cells > max_cells {
                                    truncated = true;
                                    break;
                                }
                                self.cells += cells;
                                rows.push(finished.clone());
                            }
                        }
                    }
                }
                Some(Tok::Text(_)) => {}
                None => break,
            }
        }
        if truncated {
            self.warnings
                .push(format!("cells_truncated: stopped at {max_cells} cells"));
        }
        if !rows.is_empty() {
            let width = rows.iter().map(Vec::len).max().unwrap_or(0);
            for row in &mut rows {
                row.resize(width, String::new());
            }
            self.blocks.push(Block::Table(rows));
        }
        Ok(())
    }
}

fn repeat(attrs: &Attrs<'_>, key: &str) -> usize {
    attrs
        .get(key)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_REPEAT)
}

/// 一个元素里的全部文字：`text:s`（连续空格）、`text:tab`、`text:line-break` 还原，
/// 批注和脚注编号不要，脚注正文留在原处。
fn inline(r: &mut Reader<'_>) -> Result<String, Error> {
    let mut out = String::new();
    let mut depth = 1usize;
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, a)) => match local_name(name) {
                "annotation" | "note-citation" | "bookmark-ref" | "script" => r.skip()?,
                "s" => {
                    let n = repeat(&a, "c");
                    out.extend(std::iter::repeat_n(' ', n));
                    depth += 1;
                }
                "tab" => {
                    out.push('\t');
                    depth += 1;
                }
                "line-break" => {
                    out.push('\n');
                    depth += 1;
                }
                "p" if !out.is_empty() => {
                    out.push(' ');
                    depth += 1;
                }
                _ => depth += 1,
            },
            Some(Tok::End(_)) => depth -= 1,
            Some(Tok::Text(t)) => out.push_str(&t),
            None => break,
        }
    }
    Ok(out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip;

    fn doc(format: Format, body: &str, meta: Option<&str>) -> Result<Converted, Error> {
        let content = format!(
            "<office:document-content xmlns:office=\"o\" xmlns:text=\"t\" xmlns:table=\"s\" xmlns:draw=\"d\">\
             <office:automatic-styles><style:style>x</style:style></office:automatic-styles>\
             <office:body>{body}</office:body></office:document-content>"
        );
        let mut parts: Vec<(&str, &[u8], bool)> = vec![("content.xml", content.as_bytes(), false)];
        if let Some(m) = meta {
            parts.push(("meta.xml", m.as_bytes(), false));
        }
        let bytes = zip::tests::build(&parts);
        let z = Zip::open(&bytes, &crate::Limits::default())?;
        convert(&z, format, &Options::default())
    }

    #[test]
    fn text_document() {
        let d = doc(
            Format::Odt,
            "<text:h text:outline-level=\"2\">Head</text:h>\
             <text:p>a<text:s text:c=\"2\"/>b<text:tab/>c<text:line-break/>d</text:p>\
             <text:list><text:list-item><text:p>one</text:p>\
             <text:list><text:list-item><text:p>two</text:p></text:list-item></text:list>\
             </text:list-item></text:list><text:p>after</text:p>",
            Some("<office:document-meta><dc:title>T</dc:title></office:document-meta>"),
        )
        .unwrap();
        assert_eq!(d.title.as_deref(), Some("T"));
        assert_eq!(
            d.markdown,
            "## Head\n\na  b\tc\nd\n\n- one\n    - two\n\nafter\n"
        );
    }

    #[test]
    fn spreadsheet_repeats_are_bounded() {
        let d = doc(
            Format::Ods,
            "<table:table table:name=\"S\">\
             <table:table-row><table:table-cell><text:p>a</text:p></table:table-cell>\
             <table:table-cell table:number-columns-repeated=\"2\"><text:p>b</text:p></table:table-cell>\
             <table:table-cell table:number-columns-repeated=\"1048000\"/></table:table-row>\
             <table:table-row table:number-rows-repeated=\"2\"><table:table-cell><text:p>c</text:p></table:table-cell></table:table-row>\
             <table:table-row table:number-rows-repeated=\"1048000\"><table:table-cell/></table:table-row>\
             </table:table>",
            None,
        )
        .unwrap();
        assert_eq!(d.parts, 1);
        assert_eq!(
            d.markdown,
            "## S\n\n| a | b | b |\n| --- | --- | --- |\n| c |  |  |\n| c |  |  |\n"
        );
    }

    #[test]
    fn presentation_marks_slides() {
        let d = doc(
            Format::Odp,
            "<draw:page draw:name=\"p1\"><draw:frame><draw:text-box><text:p>Hi</text:p></draw:text-box></draw:frame></draw:page>\
             <draw:page><draw:frame><draw:text-box><text:p>Bye</text:p></draw:text-box></draw:frame></draw:page>",
            None,
        )
        .unwrap();
        assert_eq!(d.parts, 2);
        assert_eq!(
            d.markdown,
            "<!-- Slide 1 -->\n\nHi\n\n<!-- Slide 2 -->\n\nBye\n"
        );
    }

    #[test]
    fn encrypted_documents_are_reported() {
        let bytes = zip::tests::build(&[(
            "META-INF/manifest.xml",
            b"<m:encryption-data/>".as_slice(),
            false,
        )]);
        let z = Zip::open(&bytes, &crate::Limits::default()).unwrap();
        assert_eq!(
            convert(&z, Format::Odt, &Options::default()),
            Err(Error::Encrypted)
        );
    }
}
