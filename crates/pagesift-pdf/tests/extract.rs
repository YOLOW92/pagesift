mod common;

use std::time::Duration;

use common::{Builder, CjkFont, Page, line, lit, paragraph, save};
use lopdf::{Object, StringFormat, dictionary};
use pagesift_pdf::{Error, Options, extract, extract_with};

const P1: &str = "The harbor was quiet in the early morning, and the fishing boats rocked gently \
against the old wooden pier while gulls circled above the water looking for scraps.";
const P2: &str = "By noon the market had filled with traders selling bread, cheese and dried \
fruit, and the narrow streets echoed with the sound of carts rolling over the cobblestones.";
const P3: &str = "In the evening the lamps were lit one by one along the promenade, and families \
walked slowly by the sea, talking about the weather and the coming festival.";

fn body(md: &str) -> Vec<&str> {
    md.split("\n\n").map(str::trim).collect()
}

#[test]
fn single_column_paragraphs() {
    let mut c = String::new();
    let mut y = 720.0;
    for p in [P1, P2, P3] {
        let (s, ny) = paragraph(11.0, 72.0, y, 14.0, 80, p);
        c.push_str(&s);
        y = ny - 12.0;
    }
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    assert_eq!(body(&doc.markdown), vec![P1, P2, P3]);
    assert!(!doc.scanned);
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(doc.metadata.page_count, 1);
}

#[test]
fn headings_by_font_size() {
    let mut c = line("F2", 24.0, 72.0, 720.0, "Harbor Town Chronicle");
    c.push_str(&line("F2", 16.0, 72.0, 680.0, "Morning"));
    c.push_str(&paragraph(11.0, 72.0, 655.0, 14.0, 80, P1).0);
    c.push_str(&line("F2", 16.0, 72.0, 590.0, "Noon"));
    c.push_str(&paragraph(11.0, 72.0, 565.0, 14.0, 80, P2).0);
    c.push_str(&line("F2", 11.0, 72.0, 500.0, "Evening notes"));
    c.push_str(&paragraph(11.0, 72.0, 480.0, 14.0, 80, P3).0);
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    assert_eq!(
        body(&doc.markdown),
        vec![
            "# Harbor Town Chronicle",
            "## Morning",
            P1,
            "## Noon",
            P2,
            "### Evening notes",
            P3
        ]
    );
}

#[test]
fn two_column_reading_order() {
    let left = format!("{P1} {P2}");
    let right = format!("{P3} The festival began at dawn with music and dancing in the square.");
    let ll = common::wrap(&left, 42);
    let rl = common::wrap(&right, 42);
    // 故意按行交错写入：左栏一行、右栏一行
    let mut c = line(
        "F2",
        20.0,
        72.0,
        730.0,
        "A Day by the Sea in Two Columns of Text",
    );
    let mut y = 690.0;
    for i in 0..ll.len().max(rl.len()) {
        if let Some(l) = rl.get(i) {
            c.push_str(&line("F1", 10.0, 320.0, y, l));
        }
        if let Some(l) = ll.get(i) {
            c.push_str(&line("F1", 10.0, 72.0, y, l));
        }
        y -= 12.0;
    }
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    let paras = body(&doc.markdown);
    assert_eq!(paras[0], "# A Day by the Sea in Two Columns of Text");
    assert_eq!(paras[1], left);
    assert_eq!(paras[2], right);
}

#[test]
fn cid_font_chinese_and_japanese() {
    let mut cjk = CjkFont::default();
    let zh1 = "港口的清晨很安静，渔船靠在旧码头边轻轻摇晃。";
    let zh2 = "中午的集市挤满了商人。";
    let ja = "日本語のテキストも正しく読めるはずです。";
    let mut c = String::new();
    c.push_str(&format!(
        "BT /F3 18 Tf 72 720 Td {} Tj ET\n",
        cjk.hex("海港日记")
    ));
    // 一段中文分两行写，行尾不加空格
    c.push_str(&format!(
        "BT /F3 11 Tf 72 680 Td {} Tj 0 -15 Td {} Tj ET\n",
        cjk.hex(&zh1[..30]),
        cjk.hex(&zh1[30..])
    ));
    c.push_str(&format!("BT /F3 11 Tf 72 630 Td {} Tj ET\n", cjk.hex(zh2)));
    c.push_str(&format!("BT /F3 11 Tf 72 600 Td {} Tj ET\n", cjk.hex(ja)));
    let mut b = Builder::new(vec![Page::text(c)]);
    b.cjk = Some(cjk);
    let doc = extract(&b.build()).unwrap();
    assert_eq!(body(&doc.markdown), vec!["# 海港日记", zh1, zh2, ja]);
}

#[test]
fn predefined_unicode_cmap_without_tounicode() {
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "STSong-Light",
        "Encoding" => "UniGB-UCS2-H",
        "DescendantFonts" => vec![Object::Dictionary(dictionary! {
            "Type" => "Font", "Subtype" => "CIDFontType0", "BaseFont" => "STSong-Light",
            "DW" => 1000,
        })],
    };
    let c = "BT /F9 12 Tf 72 700 Td <4E2D65876D4B8BD5> Tj ET\n".to_string();
    let mut b = Builder::new(vec![Page::text(c)]);
    b.extra_fonts.push(("F9".into(), font));
    let doc = extract(&b.build()).unwrap();
    assert_eq!(doc.markdown, "中文测试\n");
}

#[test]
fn differences_and_tj_spacing() {
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Custom",
        "FirstChar" => 1, "LastChar" => 3,
        "Widths" => vec![500.into(), 500.into(), 500.into()],
        "Encoding" => dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![1.into(), "eacute".into(), "fi".into(), "uni4E2D".into()],
        },
    };
    let c = "BT /F9 12 Tf 72 700 Td [<01> (t) <01> -250 <02> (ne) -40 <03>] TJ ET\n".to_string();
    let mut b = Builder::new(vec![Page::text(c)]);
    b.extra_fonts.push(("F9".into(), font));
    let doc = extract(&b.build()).unwrap();
    assert_eq!(doc.markdown, "été fine中\n");
}

#[test]
fn kerning_gap_becomes_space() {
    let c = "BT /F1 12 Tf 72 700 Td [(Hello) -300 (World) 20 (!)] TJ ET\n".to_string();
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    assert_eq!(doc.markdown, "Hello World!\n");
}

#[test]
fn header_footer_and_page_numbers_removed() {
    let texts = [P1, P2, P3];
    let pages = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut c = line("F1", 9.0, 72.0, 760.0, "Harbor Town Quarterly Report");
            c.push_str(&paragraph(11.0, 72.0, 700.0, 14.0, 80, t).0);
            c.push_str(&line(
                "F1",
                9.0,
                290.0,
                40.0,
                &format!("Page {} of 3", i + 1),
            ));
            Page::text(c)
        })
        .collect();
    let doc = extract(&Builder::new(pages).build()).unwrap();
    assert_eq!(body(&doc.markdown), vec![P1, P2, P3]);
    assert_eq!(doc.pages[1].markdown, P2);
}

#[test]
fn paragraph_continues_across_pages_and_hyphenation() {
    let p1 = line(
        "F1",
        11.0,
        72.0,
        700.0,
        "The lighthouse keeper wrote in his log that the extra-",
    ) + &line(
        "F1",
        11.0,
        72.0,
        686.0,
        "ordinary storm had passed before midnight, and the",
    ) + &line("F1", 11.0, 300.0, 40.0, "1");
    let p2 = line("F1", 11.0, 72.0, 700.0, "harbor was safe again by morning.")
        + &line("F1", 11.0, 300.0, 40.0, "2");
    let doc = extract(&Builder::new(vec![Page::text(p1), Page::text(p2)]).build()).unwrap();
    assert_eq!(
        doc.markdown,
        "The lighthouse keeper wrote in his log that the extraordinary storm had passed \
before midnight, and the harbor was safe again by morning.\n"
    );
    // 每页自己的 Markdown 不跨页合并
    assert_eq!(doc.pages[1].markdown, "harbor was safe again by morning.");
}

#[test]
fn bullets_become_list_items() {
    let mut c = line("F1", 11.0, 72.0, 700.0, "Things to pack:");
    c.push_str(&line("F1", 11.0, 72.0, 686.0, "\u{95} a warm coat"));
    c.push_str(&line("F1", 11.0, 72.0, 672.0, "\u{95} two loaves of bread"));
    let c = c.replace('\u{95}', "\\225");
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    assert_eq!(
        doc.markdown,
        "Things to pack:\n\n- a warm coat\n\n- two loaves of bread\n"
    );
}

#[test]
fn scanned_pages_are_flagged() {
    let scan = Page {
        content: String::new(),
        image: true,
    };
    let doc = extract(&Builder::new(vec![scan]).build()).unwrap();
    assert!(doc.scanned);
    assert!(doc.pages[0].scanned);
    assert_eq!(doc.pages[0].image_count, 1);
    assert_eq!(doc.markdown, "");

    // 有图有字的页面不算扫描件
    let mixed = Page {
        content: paragraph(11.0, 72.0, 700.0, 14.0, 80, P1).0,
        image: true,
    };
    let doc = extract(&Builder::new(vec![mixed]).build()).unwrap();
    assert!(!doc.scanned);
    assert!(!doc.pages[0].scanned);
}

#[test]
fn metadata_is_read() {
    let mut title = vec![0xfe, 0xff];
    for u in "海港 Chronicle".encode_utf16() {
        title.extend_from_slice(&u.to_be_bytes());
    }
    let mut b = Builder::new(vec![Page::text(line("F1", 11.0, 72.0, 700.0, "Hi"))]);
    b.info = Some(dictionary! {
        "Title" => Object::String(title, StringFormat::Hexadecimal),
        "Author" => Object::string_literal("Mira Sand"),
        "CreationDate" => Object::string_literal("D:20240315083000+08'00'"),
    });
    let doc = extract(&b.build()).unwrap();
    assert_eq!(doc.metadata.title.as_deref(), Some("海港 Chronicle"));
    assert_eq!(doc.metadata.author.as_deref(), Some("Mira Sand"));
    assert_eq!(
        doc.metadata.creation_date.as_deref(),
        Some("2024-03-15T08:30:00+08:00")
    );
}

#[test]
fn rotated_page() {
    // /Rotate 90：内容在用户空间里竖着写，显示时是横排
    let mut doc = Builder::new(vec![Page::text(
        "BT /F1 12 Tf 0 1 -1 0 100 72 Tm (Sideways text) Tj ET\n".into(),
    )])
    .document();
    let pages = doc.get_pages();
    let pid = *pages.values().next().unwrap();
    doc.get_object_mut(pid)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Rotate", 90);
    let out = extract(&save(doc)).unwrap();
    assert_eq!(out.markdown, "Sideways text\n");
}

#[test]
fn corrupt_inputs_return_errors() {
    assert!(matches!(extract(b"not a pdf"), Err(Error::Malformed(_))));
    let good = Builder::new(vec![Page::text(line("F1", 11.0, 72.0, 700.0, "Hi"))]).build();
    for cut in [10, good.len() / 3, good.len() / 2] {
        let r = extract(&good[..cut]);
        assert!(r.is_err() || r.unwrap().markdown.len() < 10);
    }
    let mut garbage = b"%PDF-1.7\n".to_vec();
    garbage.extend((0..4000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
    assert!(extract(&garbage).is_err());
}

#[test]
fn encrypted_with_password_is_rejected() {
    use lopdf::{EncryptionState, EncryptionVersion, Permissions};
    let doc = Builder::new(vec![Page::text(line("F1", 11.0, 72.0, 700.0, "Secret"))]).document();
    let encrypt = |user: &str| {
        let mut d = doc.clone();
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &d,
            owner_password: "owner",
            user_password: user,
            key_length: 128,
            permissions: Permissions::all(),
        })
        .unwrap();
        d.encrypt(&state).unwrap();
        let mut out = Vec::new();
        d.save_to(&mut out).unwrap();
        out
    };
    assert_eq!(extract(&encrypt("secret")), Err(Error::Encrypted));
    // 空用户口令（只限制权限）的文件照常提取
    assert_eq!(extract(&encrypt("")).unwrap().markdown, "Secret\n");
}

#[test]
fn page_limit_reads_only_the_first_pages() {
    let pdf = Builder::new(vec![
        Page::text(line("F1", 11.0, 72.0, 700.0, "First page")),
        Page::text(line("F1", 11.0, 72.0, 700.0, "Second page")),
    ])
    .build();
    // 页数上限按实际处理的页数算：两页的文件只读 1 页，上限 1 也不报错
    let opts = Options {
        page_limit: Some(1),
        max_pages: 1,
        ..Options::default()
    };
    let doc = extract_with(&pdf, &opts).unwrap();
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(doc.metadata.page_count, 2);
    assert!(doc.markdown.contains("First page"));
    assert!(!doc.markdown.contains("Second page"));
}

#[test]
fn limits_are_enforced() {
    let pdf = Builder::new(vec![Page::text(line("F1", 11.0, 72.0, 700.0, "Hi"))]).build();
    let opts = Options {
        timeout: Some(Duration::ZERO),
        ..Options::default()
    };
    assert_eq!(extract_with(&pdf, &opts), Err(Error::Timeout));
    let opts = Options {
        max_input_bytes: 100,
        ..Options::default()
    };
    assert_eq!(
        extract_with(&pdf, &opts),
        Err(Error::TooLarge("input size"))
    );
}

#[test]
fn pathological_content_finishes() {
    // 深层 q 嵌套、超长数组、巨大的数字和自引用的 Form XObject
    let mut c = "q ".repeat(10_000);
    c.push_str(&"[".repeat(1000));
    c.push_str(" BT /F1 1e308 Tf 1e308 1e308 Td (x) Tj ET ");
    c.push_str(&format!(
        "BT /F1 12 Tf 72 700 Td {} Tj ET",
        lit("still here")
    ));
    let doc = extract(&Builder::new(vec![Page::text(c)]).build()).unwrap();
    assert!(doc.markdown.contains("still here"));
}
