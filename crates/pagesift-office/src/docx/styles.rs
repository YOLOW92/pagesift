//! WordprocessingML 的样式（styles.xml，ECMA-376 17.7）和编号（numbering.xml，17.9）。

use std::collections::HashMap;

use crate::Error;
use crate::xml::{Reader, Tok, local_name};

#[derive(Debug, Default, Clone)]
struct Style {
    name: String,
    based_on: Option<String>,
    outline: Option<u8>,
    num: Option<(String, u8)>,
    default: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Styles {
    by_id: HashMap<String, Style>,
}

impl Styles {
    pub(crate) fn parse(xml: &str) -> Result<Styles, Error> {
        let mut styles = Styles::default();
        let mut r = Reader::new(xml);
        let mut cur: Option<(String, Style)> = None;
        let mut in_ppr = false;
        let mut num_id = None;
        let mut ilvl = 0u8;
        while let Some(t) = r.next()? {
            match t {
                Tok::Start(name, a) => match local_name(name) {
                    "style" => {
                        if a.get("type").is_some_and(|t| t == "paragraph") {
                            let id = a.get("styleId").unwrap_or_default().into_owned();
                            let default = a.get("default").is_some_and(|v| v == "1" || v == "true");
                            cur = Some((
                                id,
                                Style {
                                    default,
                                    ..Style::default()
                                },
                            ));
                        } else {
                            r.skip()?;
                        }
                    }
                    "name" if cur.is_some() => {
                        cur.as_mut().unwrap().1.name =
                            a.get("val").unwrap_or_default().into_owned();
                    }
                    "basedOn" if cur.is_some() => {
                        cur.as_mut().unwrap().1.based_on = a.get("val").map(|v| v.into_owned());
                    }
                    "pPr" => in_ppr = true,
                    // 字符格式里的同名元素不算
                    "rPr" => r.skip()?,
                    "outlineLvl" if in_ppr => {
                        if let Some(s) = cur.as_mut() {
                            s.1.outline = a.get("val").and_then(|v| v.parse().ok());
                        }
                    }
                    "numId" if in_ppr => num_id = a.get("val").map(|v| v.into_owned()),
                    "ilvl" if in_ppr => {
                        ilvl = a.get("val").and_then(|v| v.parse().ok()).unwrap_or(0)
                    }
                    _ => {}
                },
                Tok::End(name) => match local_name(name) {
                    "pPr" => in_ppr = false,
                    "style" => {
                        if let Some((id, mut s)) = cur.take() {
                            s.num = num_id.take().map(|n| (n, ilvl));
                            ilvl = 0;
                            styles.by_id.insert(id, s);
                        }
                    }
                    _ => {}
                },
                Tok::Text(_) => {}
            }
        }
        Ok(styles)
    }

    fn chain<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Style> + 'a {
        let mut next = Some(id);
        let mut steps = 0;
        std::iter::from_fn(move || {
            steps += 1;
            if steps > 32 {
                return None; // 循环继承
            }
            let s = self.by_id.get(next?)?;
            next = s.based_on.as_deref();
            Some(s)
        })
    }

    /// 样式的标题级别（1–9），不是标题时 `None`。依次看：
    /// 1. 样式继承链上的大纲级别 `outlineLvl`（0–8；默认段落样式上的不算，有的模板给 Normal 设了 0）；
    /// 2. 继承链上的内置样式名 `heading N`、`Title`（w:name 对内置样式总是英文）；
    /// 3. 自定义样式名里有 Heading / Title（不含 Subtitle、TOC、Caption），名字里有数字就用它，否则算 2 级。
    pub(crate) fn heading_level(&self, id: &str) -> Option<u8> {
        for s in self.chain(id).filter(|s| !s.default) {
            if let Some(o) = s.outline.filter(|o| *o < 9) {
                return Some(o + 1);
            }
        }
        for s in self.chain(id) {
            let name = s.name.to_ascii_lowercase();
            if let Some(n) = name
                .strip_prefix("heading ")
                .and_then(|n| n.trim().parse::<u8>().ok())
            {
                return Some(n.clamp(1, 9));
            }
            if name == "title" {
                return Some(1);
            }
        }
        let name = self.by_id.get(id)?.name.to_ascii_lowercase();
        if (name.contains("heading") || name.contains("title"))
            && !["subtitle", "toc", "caption", "table"]
                .iter()
                .any(|x| name.contains(x))
        {
            let digit = name.bytes().find(u8::is_ascii_digit).map(|d| d - b'0');
            return Some(digit.filter(|d| (1..=9).contains(d)).unwrap_or(2));
        }
        None
    }

    /// 样式继承链上定义的编号（numId, ilvl）。
    pub(crate) fn numbering(&self, id: &str) -> Option<(String, u8)> {
        self.chain(id).find_map(|s| s.num.clone())
    }

    /// 文档没指定段落样式时用的默认段落样式。
    pub(crate) fn default_id(&self) -> Option<&str> {
        self.by_id
            .iter()
            .find(|(_, s)| s.default)
            .map(|(id, _)| id.as_str())
    }
}

#[derive(Debug, Clone)]
struct Level {
    format: String,
    start: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Numbering {
    /// abstractNumId → 各级定义。
    abstracts: HashMap<String, HashMap<u8, Level>>,
    /// numId → (abstractNumId, 各级的起始值覆盖)。
    nums: HashMap<String, (String, HashMap<u8, u32>)>,
    /// 计数器：按 numId 和级别。
    counters: HashMap<String, [u32; 10]>,
}

impl Numbering {
    pub(crate) fn parse(xml: &str) -> Result<Numbering, Error> {
        let mut n = Numbering::default();
        let mut r = Reader::new(xml);
        let mut abs: Option<(String, HashMap<u8, Level>)> = None;
        let mut lvl: Option<(u8, Level)> = None;
        let mut num: Option<(String, String, HashMap<u8, u32>)> = None;
        let mut override_lvl = None;
        while let Some(t) = r.next()? {
            match t {
                Tok::Start(name, a) => {
                    let val = || a.get("val").map(|v| v.into_owned());
                    match local_name(name) {
                        "abstractNum" => {
                            abs = Some((
                                a.get("abstractNumId").unwrap_or_default().into_owned(),
                                HashMap::new(),
                            ))
                        }
                        "lvl" if num.is_none() => {
                            let i = a.get("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                            lvl = Some((
                                i,
                                Level {
                                    format: "decimal".into(),
                                    start: 1,
                                },
                            ));
                        }
                        "numFmt" => {
                            if let (Some(l), Some(v)) = (lvl.as_mut(), val()) {
                                l.1.format = v;
                            }
                        }
                        "start" => {
                            if let (Some(l), Some(v)) =
                                (lvl.as_mut(), val().and_then(|v| v.parse().ok()))
                            {
                                l.1.start = v;
                            }
                        }
                        "num" => {
                            num = Some((
                                a.get("numId").unwrap_or_default().into_owned(),
                                String::new(),
                                HashMap::new(),
                            ))
                        }
                        "abstractNumId" => {
                            if let (Some(x), Some(v)) = (num.as_mut(), val()) {
                                x.1 = v;
                            }
                        }
                        "lvlOverride" => {
                            override_lvl = a.get("ilvl").and_then(|v| v.parse::<u8>().ok())
                        }
                        "startOverride" => {
                            if let (Some(x), Some(i), Some(v)) = (
                                num.as_mut(),
                                override_lvl,
                                val().and_then(|v| v.parse().ok()),
                            ) {
                                x.2.insert(i, v);
                            }
                        }
                        // 嵌套的 rPr/pPr 里没有要的东西
                        "rPr" | "pPr" => r.skip()?,
                        _ => {}
                    }
                }
                Tok::End(name) => match local_name(name) {
                    "lvl" => {
                        if let (Some((i, l)), Some(a)) = (lvl.take(), abs.as_mut()) {
                            a.1.insert(i, l);
                        }
                    }
                    "abstractNum" => {
                        if let Some((id, levels)) = abs.take() {
                            n.abstracts.insert(id, levels);
                        }
                    }
                    "num" => {
                        if let Some((id, a, o)) = num.take() {
                            n.nums.insert(id, (a, o));
                        }
                    }
                    _ => {}
                },
                Tok::Text(_) => {}
            }
        }
        Ok(n)
    }

    /// 一个编号段落的列表记号：(有序？, 编号)。`numId` 为 0 或格式是 `none` 时不是列表项。
    /// 计数按 numId 分开，出现上一级的项时下面各级重新计数。
    pub(crate) fn next(&mut self, num_id: &str, ilvl: u8) -> Option<(bool, u32)> {
        if num_id == "0" {
            return None;
        }
        let ilvl = ilvl.min(9);
        let (abs, overrides) = self.nums.get(num_id)?;
        let level = self
            .abstracts
            .get(abs)
            .and_then(|l| l.get(&ilvl))
            .cloned()
            .unwrap_or(Level {
                format: "decimal".into(),
                start: 1,
            });
        if level.format == "none" {
            return None;
        }
        let start = overrides.get(&ilvl).copied().unwrap_or(level.start);
        let c = self.counters.entry(num_id.to_string()).or_insert([0; 10]);
        c[usize::from(ilvl)] = if c[usize::from(ilvl)] == 0 {
            start
        } else {
            c[usize::from(ilvl)] + 1
        };
        for deeper in &mut c[usize::from(ilvl) + 1..] {
            *deeper = 0;
        }
        Some((level.format != "bullet", c[usize::from(ilvl)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: &str = r#"<w:styles xmlns:w="w">
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="AgencyH2"><w:name w:val="Heading 2 (Agency)"/><w:basedOn w:val="Heading2"/></w:style>
<w:style w:type="paragraph" w:styleId="Sec"><w:name w:val="EM Section Heading"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="7"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Sub"><w:name w:val="Subtitle"/></w:style>
<w:style w:type="paragraph" w:styleId="List"><w:name w:val="List Bullet"/><w:pPr><w:numPr><w:numId w:val="3"/></w:numPr></w:pPr></w:style>
<w:style w:type="character" w:styleId="Strong"><w:name w:val="heading 1"/></w:style>
</w:styles>"#;

    #[test]
    fn heading_levels_follow_outline_names_and_inheritance() {
        let s = Styles::parse(STYLES).unwrap();
        assert_eq!(s.heading_level("Normal"), None);
        assert_eq!(s.heading_level("Heading2"), Some(2));
        assert_eq!(s.heading_level("AgencyH2"), Some(2));
        assert_eq!(s.heading_level("Sec"), Some(2));
        assert_eq!(s.heading_level("Sub"), None);
        assert_eq!(s.heading_level("Strong"), None);
        assert_eq!(s.numbering("List"), Some(("3".into(), 0)));
        assert_eq!(s.default_id(), Some("Normal"));
    }

    #[test]
    fn numbering_counts_restarts_and_bullets() {
        let xml = r#"<w:numbering xmlns:w="w">
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/></w:lvl>
<w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="10"/></w:lvlOverride></w:num>
</w:numbering>"#;
        let mut n = Numbering::parse(xml).unwrap();
        assert_eq!(n.next("1", 0), Some((true, 1)));
        assert_eq!(n.next("1", 1), Some((false, 1)));
        assert_eq!(n.next("1", 0), Some((true, 2)));
        assert_eq!(n.next("2", 0), Some((true, 10)));
        assert_eq!(n.next("2", 0), Some((true, 11)));
        assert_eq!(n.next("0", 0), None);
        assert_eq!(n.next("99", 0), None);
    }
}
