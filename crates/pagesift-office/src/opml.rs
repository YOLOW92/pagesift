//! OPML 大纲：`<outline>` 的嵌套转成嵌套列表，订阅源（`xmlUrl`、`htmlUrl`、`url`）写成链接。

use crate::md::{self, Block};
use crate::xml::{self, Reader, Tok, local_name};
use crate::{Converted, Error, Options, check_deadline};

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = xml::decode(bytes.to_vec());
    let mut reader = Reader::new(&text);
    let mut blocks = Vec::new();
    let mut title = None;
    let mut seen_root = false;
    let mut depth = 0usize;
    let mut items = 0usize;
    while let Some(tok) = reader.next()? {
        match tok {
            Tok::Start(name, attrs) => match local_name(name) {
                "opml" => seen_root = true,
                "title" if title.is_none() => {
                    let t = reader.text_until_end()?;
                    title = Some(t.trim().to_owned()).filter(|t| !t.is_empty());
                }
                "outline" => {
                    if items.is_multiple_of(256) {
                        check_deadline(opts)?;
                    }
                    items += 1;
                    let label = attrs
                        .get("text")
                        .or_else(|| attrs.get("title"))
                        .unwrap_or_default();
                    let url = ["xmlUrl", "htmlUrl", "url"]
                        .iter()
                        .find_map(|k| attrs.get(k));
                    let label = md::escape(label.trim());
                    let line = match url {
                        Some(u) => {
                            let u = u.replace(' ', "%20").replace(')', "%29");
                            format!("[{}]({u})", if label.is_empty() { &u } else { &label })
                        }
                        None => label,
                    };
                    blocks.push(Block::Item {
                        level: u8::try_from(depth).unwrap_or(u8::MAX),
                        ordered: false,
                        number: 0,
                        text: line,
                    });
                    depth += 1;
                }
                _ => {}
            },
            Tok::End(name) if local_name(name) == "outline" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if !seen_root {
        return Err(Error::Malformed("not an OPML document".into()));
    }
    if let Some(t) = &title {
        blocks.insert(0, Block::Heading(1, md::escape(t)));
    }
    Ok(Converted {
        markdown: md::render(&blocks),
        title,
        parts: items,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_become_nested_lists() {
        let x =
            br#"<?xml version="1.0"?><opml version="2.0"><head><title>Feeds</title></head><body>
            <outline text="Tech"><outline text="Blog" xmlUrl="https://a.example/rss"/></outline>
            <outline title="Plain"/></body></opml>"#;
        let d = convert(x, &Options::default()).unwrap();
        assert_eq!(d.title.as_deref(), Some("Feeds"));
        assert_eq!(d.parts, 3);
        assert_eq!(
            d.markdown,
            "# Feeds\n\n- Tech\n    - [Blog](https://a.example/rss)\n- Plain\n"
        );
    }

    #[test]
    fn other_xml_is_rejected() {
        assert!(matches!(
            convert(b"<rss/>", &Options::default()),
            Err(Error::Malformed(_))
        ));
    }
}
