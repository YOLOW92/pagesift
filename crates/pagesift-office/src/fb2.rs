//! FictionBook 2：`description` 里取书名和作者，`body` 按 section 嵌套转标题和段落，
//! 诗（poem）的每行保留换行，表格转 Markdown 表格。`binary`（内嵌图片）不输出。

use crate::md::{self, Block};
use crate::xml::{self, Reader, Tok, local_name};
use crate::{Converted, Error, Options, budget, check_deadline};

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = xml::decode(bytes.to_vec());
    let mut r = Reader::new(&text);
    let mut seen_root = false;
    let mut title = None;
    let mut metadata = Vec::new();
    let mut blocks = Vec::new();
    while let Some(tok) = r.next()? {
        let Tok::Start(name, _) = tok else { continue };
        match local_name(name) {
            "FictionBook" => seen_root = true,
            "title-info" => read_info(&mut r, &mut title, &mut metadata)?,
            "body" => read_body(&mut r, &mut blocks, opts)?,
            "binary" | "stylesheet" | "publish-info" | "document-info" | "custom-info" => {
                r.skip()?;
            }
            _ => {}
        }
    }
    if !seen_root {
        return Err(Error::Malformed("not a FictionBook document".into()));
    }
    let parts = blocks.len();
    Ok(Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        title,
        metadata,
        parts,
        ..Default::default()
    })
}

fn read_info(
    r: &mut Reader<'_>,
    title: &mut Option<String>,
    metadata: &mut Vec<(String, String)>,
) -> Result<(), Error> {
    let mut depth = 1usize;
    let mut author: Vec<String> = Vec::new();
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, _)) => match local_name(name) {
                "book-title" => {
                    *title = Some(r.text_until_end()?.trim().to_owned()).filter(|t| !t.is_empty());
                }
                "lang" => {
                    let v = r.text_until_end()?;
                    metadata.push(("language".into(), v.trim().to_owned()));
                }
                "first-name" | "middle-name" | "last-name" => {
                    author.push(r.text_until_end()?.trim().to_owned());
                }
                "annotation" | "coverpage" => r.skip()?,
                _ => depth += 1,
            },
            Some(Tok::End(name)) => {
                if local_name(name) == "author" && !author.is_empty() {
                    metadata.push(("creator".into(), author.join(" ")));
                    author.clear();
                }
                depth -= 1;
            }
            Some(Tok::Text(_)) => {}
            None => break,
        }
    }
    Ok(())
}

fn read_body(r: &mut Reader<'_>, blocks: &mut Vec<Block>, opts: &Options<'_>) -> Result<(), Error> {
    let mut depth = 1usize;
    let mut sections = 0usize;
    let mut n = 0usize;
    while depth > 0 {
        n += 1;
        if n.is_multiple_of(256) {
            check_deadline(opts)?;
        }
        match r.next()? {
            Some(Tok::Start(name, _)) => match local_name(name) {
                "section" => {
                    sections += 1;
                    depth += 1;
                }
                "title" => {
                    let text = inline_block(r)?;
                    if !text.is_empty() {
                        let level = u8::try_from(sections + 1).unwrap_or(6).clamp(1, 6);
                        blocks.push(Block::Heading(level, md::escape(&text)));
                    }
                }
                "p" | "subtitle" | "text-author" | "v" => {
                    let text = r.text_until_end()?;
                    let text = text.trim();
                    budget::spend(text.len());
                    if !text.is_empty() {
                        blocks.push(Block::Para(md::escape(text)));
                    }
                }
                "stanza" => {
                    let text = read_stanza(r)?;
                    if !text.is_empty() {
                        blocks.push(Block::Para(text));
                    }
                }
                "table" => {
                    let rows = read_table(r)?;
                    if !rows.is_empty() {
                        blocks.push(Block::Table(rows));
                    }
                }
                "image" | "empty-line" => r.skip()?,
                _ => depth += 1,
            },
            Some(Tok::End(name)) => {
                if local_name(name) == "section" {
                    sections = sections.saturating_sub(1);
                }
                depth -= 1;
            }
            Some(Tok::Text(_)) => {}
            None => break,
        }
    }
    Ok(())
}

/// `<title>` 里是一到多个 `<p>`：合成一行。
fn inline_block(r: &mut Reader<'_>) -> Result<String, Error> {
    let mut parts = Vec::new();
    let mut depth = 1usize;
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, _)) if local_name(name) == "p" => {
                let t = r.text_until_end()?;
                let t = t.trim();
                if !t.is_empty() {
                    parts.push(t.to_owned());
                }
            }
            Some(Tok::Start(..)) => depth += 1,
            Some(Tok::End(_)) => depth -= 1,
            Some(Tok::Text(t)) => {
                let t = t.trim();
                if !t.is_empty() {
                    parts.push(t.to_owned());
                }
            }
            None => break,
        }
    }
    Ok(parts.join(" "))
}

fn read_stanza(r: &mut Reader<'_>) -> Result<String, Error> {
    let mut lines = Vec::new();
    let mut depth = 1usize;
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, _)) if local_name(name) == "v" => {
                let t = r.text_until_end()?;
                let t = t.trim();
                budget::spend(t.len());
                lines.push(md::escape(t));
            }
            Some(Tok::Start(..)) => depth += 1,
            Some(Tok::End(_)) => depth -= 1,
            Some(Tok::Text(_)) => {}
            None => break,
        }
    }
    Ok(lines.join("\n"))
}

fn read_table(r: &mut Reader<'_>) -> Result<Vec<Vec<String>>, Error> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut depth = 1usize;
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, _)) => match local_name(name) {
                "th" | "td" => {
                    let t = r.text_until_end()?;
                    budget::spend(t.len());
                    row.push(md::escape(t.trim()));
                }
                _ => depth += 1,
            },
            Some(Tok::End(name)) => {
                if local_name(name) == "tr" && !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
                depth -= 1;
            }
            Some(Tok::Text(_)) => {}
            None => break,
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_structure() {
        let x = "<?xml version=\"1.0\" encoding=\"utf-8\"?><FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\">\
          <description><title-info><author><first-name>A</first-name><last-name>B</last-name></author>\
          <book-title>Book</book-title><annotation><p>skip</p></annotation><lang>ru</lang></title-info></description>\
          <body><title><p>Book</p></title><section><title><p>One</p></title><p>Text <emphasis>here</emphasis></p>\
          <section><title><p>Sub</p></title><poem><stanza><v>l1</v><v>l2</v></stanza></poem></section>\
          <table><tr><th>h</th></tr><tr><td>c</td></tr></table></section></body>\
          <binary id=\"x\">QUJD</binary></FictionBook>";
        let d = convert(x.as_bytes(), &Options::default()).unwrap();
        assert_eq!(d.title.as_deref(), Some("Book"));
        assert!(d.metadata.contains(&("creator".into(), "A B".into())));
        assert!(d.metadata.contains(&("language".into(), "ru".into())));
        assert_eq!(
            d.markdown,
            "# Book\n\n## One\n\nText here\n\n### Sub\n\nl1\nl2\n\n| h |\n| --- |\n| c |\n"
        );
    }

    #[test]
    fn other_xml_is_rejected() {
        assert!(matches!(
            convert(b"<a/>", &Options::default()),
            Err(Error::Malformed(_))
        ));
    }
}
