//! OPC（ECMA-376 第 2 部分）：部件之间的关系（`_rels/*.rels`）、目标路径解析、核心属性。

use std::collections::HashMap;

use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error};

pub(crate) const OFFICE_DOCUMENT: &str = "/officeDocument";

#[derive(Debug, Clone)]
pub(crate) struct Rel {
    /// 关系类型 URI 的最后一段（`.../relationships/image` → `image`）。
    pub kind: String,
    /// 内部部件时是解析好的包内路径（不带开头的 `/`）；外部时是原样的地址。
    pub target: String,
    pub external: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Rels {
    by_id: HashMap<String, Rel>,
    order: Vec<String>,
}

impl Rels {
    /// 解析关系部件。`base` 是源部件所在的目录（例如 `word`），相对目标按它解析。
    pub(crate) fn parse(xml: &str, base: &str) -> Result<Rels, Error> {
        let mut rels = Rels::default();
        let mut r = Reader::new(xml);
        while let Some(t) = r.next()? {
            let Tok::Start(name, a) = t else { continue };
            if local_name(name) != "Relationship" {
                continue;
            }
            let (Some(id), Some(target)) = (a.get("Id"), a.get("Target")) else {
                continue;
            };
            let external = a.get("TargetMode").is_some_and(|m| m == "External");
            let kind = a
                .get("Type")
                .map(|t| t.rsplit('/').next().unwrap_or("").to_string())
                .unwrap_or_default();
            let target = if external {
                target.into_owned()
            } else {
                resolve(base, &target)
            };
            rels.order.push(id.to_string());
            rels.by_id.insert(
                id.into_owned(),
                Rel {
                    kind,
                    target,
                    external,
                },
            );
        }
        Ok(rels)
    }

    /// 读部件 `part` 的关系（`dir/_rels/name.rels`）；没有关系部件时为空。
    pub(crate) fn of(zip: &Zip<'_>, part: &str) -> Result<Rels, Error> {
        let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
        let path = if dir.is_empty() {
            format!("_rels/{name}.rels")
        } else {
            format!("{dir}/_rels/{name}.rels")
        };
        match zip.read_text(&path)? {
            Some(xml) => Rels::parse(&xml, dir),
            None => Ok(Rels::default()),
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Rel> {
        self.by_id.get(id)
    }

    /// 按出现顺序找第一个某类型的关系。
    pub(crate) fn first(&self, kind: &str) -> Option<&Rel> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .find(|r| r.kind == kind)
    }

    /// 包的主部件（`officeDocument` 关系）。
    pub(crate) fn office_document(&self) -> Option<String> {
        self.first(&OFFICE_DOCUMENT[1..]).map(|r| r.target.clone())
    }
}

/// 把相对目标按源目录解析成包内路径：处理 `..`、`.`、以 `/` 开头的绝对路径和 `%20` 之类的转义。
pub(crate) fn resolve(base: &str, target: &str) -> String {
    let target = percent_decode(target);
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|p| !p.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 主部件路径：先看 `_rels/.rels`，没有时用默认位置。
pub(crate) fn main_part(zip: &Zip<'_>, default: &str) -> Result<String, Error> {
    let found = Rels::of(zip, "")?.office_document();
    Ok(found
        .filter(|p| zip.contains(p))
        .unwrap_or_else(|| default.to_string()))
}

/// 核心属性（docProps/core.xml）：标题进 `title`，作者、主题等进 `metadata`。
pub(crate) fn core_properties(zip: &Zip<'_>, out: &mut Converted) -> Result<(), Error> {
    let Some(xml) = zip.read_text("docProps/core.xml")? else {
        return Ok(());
    };
    let mut r = Reader::new(&xml);
    while let Some(t) = r.next()? {
        let Tok::Start(name, _) = t else { continue };
        let key = match local_name(name) {
            k @ ("title" | "creator" | "subject" | "description" | "language" | "keywords"
            | "created" | "modified") => k,
            _ => continue,
        };
        let value = crate::md::squeeze(&r.text_until_end()?);
        if value.is_empty() {
            continue;
        }
        if key == "title" {
            out.title = Some(value);
        } else {
            out.metadata.push((key.to_string(), value));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute_targets() {
        assert_eq!(resolve("word", "media/image1.png"), "word/media/image1.png");
        assert_eq!(
            resolve("ppt/slides", "../media/a%20b.png"),
            "ppt/media/a b.png"
        );
        assert_eq!(
            resolve("xl", "/xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(resolve("", "word/document.xml"), "word/document.xml");
        assert_eq!(resolve("a", "../../../x"), "x");
    }

    #[test]
    fn parses_relationships() {
        let xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/i.png"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://e.com/?a=1&amp;b=2" TargetMode="External"/>
</Relationships>"#;
        let r = Rels::parse(xml, "word").unwrap();
        assert_eq!(r.get("rId1").unwrap().target, "word/media/i.png");
        let link = r.get("rId2").unwrap();
        assert!(link.external);
        assert_eq!(link.target, "https://e.com/?a=1&b=2");
        assert_eq!(r.first("image").unwrap().kind, "image");
    }
}
