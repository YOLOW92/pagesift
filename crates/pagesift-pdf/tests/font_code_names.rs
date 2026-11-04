//! 内建编码的字形名只是字符码（`C65`）的字体：曾经整页解不出字。
mod common;

use common::{Builder, Page, line, save};
use lopdf::{Object, Stream, dictionary};
use pagesift_pdf::extract;

fn index(items: &[&[u8]]) -> Vec<u8> {
    let mut out = (items.len() as u16).to_be_bytes().to_vec();
    out.push(2);
    let mut off = 1u16;
    out.extend(off.to_be_bytes());
    for it in items {
        off += it.len() as u16;
        out.extend(off.to_be_bytes());
    }
    out.extend(items.concat());
    out
}

/// 只含编码信息的 CFF 字体：`glyphs` 是 (字符码, 字形名)。
fn cff(glyphs: &[(u8, String)]) -> Vec<u8> {
    let int = |v: usize| {
        let b = (v as i32).to_be_bytes();
        vec![29, b[0], b[1], b[2], b[3]]
    };
    const FIRST_CUSTOM_SID: u16 = 391;
    let header = [1u8, 0, 4, 1];
    let names = index(&[b"F"]);
    let strings: Vec<&[u8]> = glyphs.iter().map(|(_, n)| n.as_bytes()).collect();
    let strings = index(&strings);
    let gsubrs = 0u16.to_be_bytes();
    let top_index_len = 2 + 1 + 2 * 2 + 18;
    let charset_off = header.len() + names.len() + top_index_len + strings.len() + gsubrs.len();
    let mut charset = vec![0u8];
    for i in 0..glyphs.len() as u16 {
        charset.extend((FIRST_CUSTOM_SID + i).to_be_bytes());
    }
    let mut encoding = vec![0u8, glyphs.len() as u8];
    encoding.extend(glyphs.iter().map(|(code, _)| *code));
    let enc_off = charset_off + charset.len();
    let cs_off = enc_off + encoding.len();
    let endchar: &[u8] = &[14];
    let charstrings = index(&vec![endchar; glyphs.len() + 1]);
    let mut top = int(charset_off);
    top.push(15);
    top.extend(int(enc_off));
    top.push(16);
    top.extend(int(cs_off));
    top.push(17);
    let mut font = header.to_vec();
    font.extend(names);
    font.extend(index(&[&top]));
    font.extend(strings);
    font.extend(gsubrs);
    font.extend(charset);
    font.extend(encoding);
    font.extend(charstrings);
    font
}

/// 把 `content` 用嵌入了这份 CFF 的字体 F9 排出来，提取成 Markdown。
fn extract_with_font(glyphs: &[(u8, String)], content: String) -> String {
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "CodeNames",
        "FirstChar" => 0, "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
    };
    let mut builder = Builder::new(vec![Page::text(content)]);
    builder.extra_fonts.push(("F9".to_string(), font));
    let mut doc = builder.document();
    let program = doc.add_object(Stream::new(
        dictionary! { "Subtype" => "Type1C" },
        cff(glyphs),
    ));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "CodeNames", "Flags" => 4,
        "FontFile3" => program,
    });
    for obj in doc.objects.values_mut() {
        if let Object::Dictionary(d) = obj
            && d.get(b"BaseFont")
                .is_ok_and(|n| n.as_name().is_ok_and(|n| n == b"CodeNames"))
        {
            d.set("FontDescriptor", descriptor);
        }
    }
    extract(&save(doc)).unwrap().markdown
}

fn code_names(codes: impl Iterator<Item = u8>) -> Vec<(u8, String)> {
    codes.map(|c| (c, format!("C{c}"))).collect()
}

#[test]
fn names_repeating_the_code_use_the_base_encoding() {
    let glyphs = code_names((32..=126).chain([0xb1, 0xae]));
    let md = extract_with_font(
        &glyphs,
        "BT /F9 11 Tf 72 720 Td (Harbor pages 12\\26133 on speci\\256c tides) Tj ET\n".to_string(),
    );
    assert_eq!(md.trim(), "Harbor pages 12\u{2013}33 on specific tides");
}

/// 有一个字形名和码对不上，就不能说名字只是在重复码：符号字体常这样（名字是编号，
/// 字形却不是基础编码里那个字），按基础编码读会把符号读成不相干的字母。
#[test]
fn names_not_matching_the_code_are_not_read_as_base_encoding() {
    let mut glyphs = code_names(65..=70);
    glyphs.push((255, "C0".to_string()));
    let mut content = line("F1", 11.0, 72.0, 720.0, "Harbor");
    content.push_str("BT /F9 11 Tf 72 700 Td (ABC) Tj ET\n");
    let md = extract_with_font(&glyphs, content);
    assert_eq!(md.trim(), "Harbor");
}

/// TeX 的图形字体（XYATIP、LASY、LCIRCLE）也用码命名，但字形是箭头和圆弧，不是字。
#[test]
fn graphic_fonts_named_by_code_stay_unread() {
    let glyphs: Vec<(u8, String)> = (65..=70).map(|c| (c, format!("a{c}"))).collect();
    let mut content = line("F1", 11.0, 72.0, 720.0, "Harbor");
    content.push_str("BT /F9 11 Tf 72 700 Td (ABC) Tj ET\n");
    let md = extract_with_font(&glyphs, content);
    assert_eq!(md.trim(), "Harbor");
}
