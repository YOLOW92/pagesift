//! 结构化 XML 文档：DocBook、JATS（期刊文章）共用一个遍历，SVG 只取文字。
//! 只认正文元素（标题、段落、列表、表格、代码），其余标签之外的文字不输出，出版元数据不混进正文。

use crate::md::{self, Block};
use crate::text::fence_into;
use crate::xml::{self, Reader, Tok, local_name};
use crate::{Converted, Error, Options, budget, check_deadline};

fn finish(blocks: Vec<Block>, title: Option<String>) -> Converted {
    let parts = blocks.len();
    Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        title,
        parts,
        ..Default::default()
    }
}

/// DocBook 4/5 和 JATS。
pub(crate) fn convert_article(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = xml::decode(bytes.to_vec());
    let mut r = Reader::new(&text);
    let mut blocks: Vec<Block> = Vec::new();
    let mut title: Option<String> = None;
    let (mut sections, mut lists, mut steps) = (0usize, 0usize, 0usize);
    // 列表项的序号：每层一个，有序列表才用
    let mut ordered: Vec<(bool, u32)> = Vec::new();
    while let Some(tok) = r.next()? {
        steps += 1;
        if steps % 256 == 0 {
            check_deadline(opts)?;
            if budget::exhausted() {
                break;
            }
        }
        match tok {
            Tok::Start(name, _) => match local_name(name) {
                "journal-meta" | "contrib-group" | "aff" | "author-notes" | "permissions"
                | "history" | "kwd-group" | "funding-group" | "indexterm" | "remark"
                | "graphic" | "imagedata" | "imageobject" | "mediaobject" | "tex-math"
                | "copyright" | "legalnotice" | "authorgroup" | "author" | "editor" | "pubdate"
                | "date" | "revhistory" | "biblioid" | "publisher" | "volume" | "issue"
                | "fpage" | "lpage" | "pub-date" | "article-id" | "article-categories" => {
                    r.skip()?
                }
                "book" | "article" | "chapter" | "section" | "sect1" | "sect2" | "sect3"
                | "sect4" | "sect5" | "simplesect" | "appendix" | "preface" | "part"
                | "refsection" | "sec" | "app" | "ack" | "abstract" | "ref-list" | "body" => {
                    if !matches!(local_name(name), "body" | "abstract") {
                        sections += 1;
                    }
                }
                "title" | "article-title" | "subtitle" | "trans-title" => {
                    let t = squeeze(&r.text_until_end()?);
                    if t.is_empty() {
                        continue;
                    }
                    budget::spend(t.len());
                    let level = u8::try_from(sections.max(1)).unwrap_or(6).clamp(1, 6);
                    if title.is_none() {
                        title = Some(t.clone());
                    }
                    blocks.push(Block::Heading(level, md::escape(&t)));
                }
                "para" | "simpara" | "p" | "term" | "mixed-citation" | "element-citation"
                | "citation" | "bibliomixed" => {
                    let t = squeeze(&r.text_until_end()?);
                    if t.is_empty() {
                        continue;
                    }
                    budget::spend(t.len());
                    let text = md::escape(&t);
                    blocks.push(if lists > 0 {
                        let (is_ordered, number) =
                            ordered.last_mut().map_or((false, 0), |(o, n)| {
                                *n += 1;
                                (*o, *n)
                            });
                        Block::Item {
                            level: u8::try_from(lists - 1).unwrap_or(u8::MAX),
                            ordered: is_ordered,
                            number,
                            text,
                        }
                    } else {
                        Block::Para(text)
                    });
                }
                n @ ("itemizedlist" | "orderedlist" | "variablelist" | "simplelist" | "list"
                | "def-list") => {
                    lists += 1;
                    ordered.push((n == "orderedlist", 0));
                }
                "programlisting" | "screen" | "literallayout" | "synopsis" | "code"
                | "preformat" => {
                    let body = r.text_until_end()?;
                    budget::spend(body.len());
                    let mut fenced = String::new();
                    fence_into(&mut fenced, "", &body);
                    blocks.push(Block::Raw(fenced.trim_end().to_owned()));
                }
                "table" | "informaltable" => {
                    let rows = table(&mut r)?;
                    if !rows.is_empty() {
                        blocks.push(Block::Table(rows));
                    }
                }
                _ => {}
            },
            Tok::End(name) => match local_name(name) {
                "book" | "article" | "chapter" | "section" | "sect1" | "sect2" | "sect3"
                | "sect4" | "sect5" | "simplesect" | "appendix" | "preface" | "part"
                | "refsection" | "sec" | "app" | "ack" | "ref-list" => {
                    sections = sections.saturating_sub(1);
                }
                "itemizedlist" | "orderedlist" | "variablelist" | "simplelist" | "list"
                | "def-list" => {
                    lists = lists.saturating_sub(1);
                    ordered.pop();
                }
                _ => {}
            },
            Tok::Text(_) => {}
        }
    }
    Ok(finish(blocks, title))
}

/// 表格：DocBook 的 row/entry 和 JATS 的 tr/td/th。表题另成一段放在前面的逻辑交给调用方的遍历。
fn table(r: &mut Reader<'_>) -> Result<Vec<Vec<String>>, Error> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut depth = 1usize;
    while depth > 0 {
        match r.next()? {
            Some(Tok::Start(name, _)) => match local_name(name) {
                "entry" | "td" | "th" => {
                    let t = squeeze(&r.text_until_end()?);
                    budget::spend(t.len());
                    row.push(md::escape(&t));
                }
                "colspec" | "col" | "colgroup" | "spanspec" => r.skip()?,
                _ => depth += 1,
            },
            Some(Tok::End(name)) => {
                depth -= 1;
                if matches!(local_name(name), "row" | "tr") && !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
            }
            Some(Tok::Text(_)) => {}
            None => break,
        }
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(width, String::new());
    }
    Ok(rows)
}

/// 连续空白合成一个：XML 的缩进和换行不是内容。
fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// SVG：`title`、`desc`、`text` 里的文字，每个元素一段。路径和样式都不要。
pub(crate) fn convert_svg(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = xml::decode(bytes.to_vec());
    let mut r = Reader::new(&text);
    let mut blocks = Vec::new();
    let mut title = None;
    let mut steps = 0usize;
    while let Some(tok) = r.next()? {
        steps += 1;
        if steps.is_multiple_of(256) {
            check_deadline(opts)?;
        }
        if let Tok::Start(name, _) = tok {
            match local_name(name) {
                n @ ("title" | "desc" | "text") => {
                    let t = squeeze(&r.text_until_end()?);
                    if !t.is_empty() {
                        budget::spend(t.len());
                        if n == "title" && title.is_none() {
                            title = Some(t.clone());
                        }
                        blocks.push(Block::Para(md::escape(&t)));
                    }
                }
                "style" | "script" | "defs" | "metadata" => r.skip()?,
                _ => {}
            }
        }
    }
    Ok(finish(blocks, title))
}

/// XML 根元素的名字（跳过声明、注释、DOCTYPE）。
pub(crate) fn root_name(bytes: &[u8]) -> Option<&str> {
    let head = &bytes[..bytes.len().min(16 << 10)];
    let mut i = 0;
    while let Some(off) = head[i..].iter().position(|&b| b == b'<') {
        let at = i + off + 1;
        match head.get(at) {
            Some(b'?') | Some(b'!') => {
                // 注释和 DOCTYPE 里可能有 `>`，但根元素的名字不会被它们骗到：继续找下一个 `<`
                i = at;
            }
            Some(_) => {
                let rest = &head[at..];
                let end = rest
                    .iter()
                    .position(|b| matches!(b, b' ' | b'>' | b'/' | b'\n' | b'\r' | b'\t'))?;
                let name = std::str::from_utf8(&rest[..end]).ok()?;
                return Some(local_name(name));
            }
            None => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(x: &str) -> Converted {
        convert_article(x.as_bytes(), &Options::default()).unwrap()
    }

    #[test]
    fn docbook_sections_lists_tables_code() {
        let d = run(
            r#"<?xml version="1.0"?><article xmlns="http://docbook.org/ns/docbook"><info><title>Doc</title><author><personname>A</personname></author></info>
          <section><title>One</title><para>Text <emphasis>here</emphasis>
            wrapped</para>
            <itemizedlist><listitem><para>a</para></listitem><listitem><para>b</para></listitem></itemizedlist>
            <orderedlist><listitem><para>x</para></listitem><listitem><para>y</para></listitem></orderedlist>
            <programlisting>code()  &lt;1&gt;</programlisting>
            <informaltable><tgroup cols="2"><colspec/><thead><row><entry>h1</entry><entry>h2</entry></row></thead>
            <tbody><row><entry>c1</entry><entry>c2</entry></row></tbody></tgroup></informaltable>
          </section></article>"#,
        );
        assert_eq!(d.title.as_deref(), Some("Doc"));
        assert_eq!(
            d.markdown,
            "# Doc\n\n## One\n\nText here wrapped\n\n- a\n- b\n1. x\n2. y\n\n```\ncode()  <1>\n```\n\n| h1 | h2 |\n| --- | --- |\n| c1 | c2 |\n"
        );
    }

    #[test]
    fn jats_keeps_the_article_and_skips_publisher_data() {
        let d = run(
            r#"<article><front><journal-meta><journal-title>J</journal-title></journal-meta>
          <article-meta><title-group><article-title>Paper</article-title></title-group>
          <contrib-group><contrib><name>N</name></contrib></contrib-group>
          <abstract><p>Short.</p></abstract></article-meta></front>
          <body><sec><title>Intro</title><p>Body <xref>1</xref>.</p>
          <list><list-item><p>item</p></list-item></list></sec></body></article>"#,
        );
        assert_eq!(d.title.as_deref(), Some("Paper"));
        assert_eq!(
            d.markdown,
            "# Paper\n\nShort.\n\n## Intro\n\nBody 1.\n\n- item\n"
        );
    }

    #[test]
    fn svg_keeps_only_words() {
        let d = convert_svg(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><title>Chart</title><style>.a{}</style>
              <path d="M0 0"/><text x="1"><tspan>Sales</tspan> <tspan>2024</tspan></text></svg>"#,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(d.title.as_deref(), Some("Chart"));
        assert_eq!(d.markdown, "Chart\n\nSales 2024\n");
    }

    #[test]
    fn root_names() {
        assert_eq!(root_name(b"<?xml?><!-- c --><svg:svg a='1'>"), Some("svg"));
        assert_eq!(root_name(b"<!DOCTYPE x>\n<opml>"), Some("opml"));
        assert_eq!(root_name(b"no tags"), None);
    }
}
