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

/// 手写的 PDF：Catalog、Pages、字体在前，每页一个页面对象和内容流在后，xref 在文件尾。
/// 对象顺序固定，截断时丢的是后面的页。
fn plain_pdf(pages: &[&str]) -> Vec<u8> {
    let n = pages.len();
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!(
            "<< /Type /Pages /Count {n} /Kids [{}] >>",
            (0..n)
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    for (i, text) in pages.iter().enumerate() {
        let content = format!("BT /F1 12 Tf 72 720 Td {} Tj ET", lit(text));
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// 截断的文件：xref 丢了、后两页的对象丢了。前三页照常恢复，结果里要说明少了两页。
#[test]
fn truncated_file_keeps_its_pages_and_warns() {
    let whole = plain_pdf(&[
        "Page one.",
        "Page two.",
        "Page three.",
        "Page four.",
        "Page five.",
    ]);
    assert!(extract(&whole).unwrap().warnings.is_empty());
    // 切在第 4 页的页面对象里（对象号 10）
    let cut = whole.windows(9).position(|w| w == b"10 0 obj\n").unwrap() + 20;
    let doc = extract(&whole[..cut]).unwrap();
    assert_eq!(doc.pages.len(), 3);
    assert_eq!(doc.metadata.page_count, 3);
    assert!(doc.markdown.contains("Page three.") && !doc.markdown.contains("Page four."));
    assert_eq!(doc.warnings.len(), 2, "{:?}", doc.warnings);
    assert!(doc.warnings[0].starts_with("pdf_repaired: the cross-reference table was unusable"));
    assert_eq!(
        doc.warnings[1],
        "pages_missing: 2 of 5 pages could not be recovered from the damaged file; 3 converted"
    );
}

/// 页树根丢了的截断文件（线性化的 PDF 切掉后半截时常见：第一页的对象在文件头，/Pages 在后面）：
/// 剩下的页面对象按线性化字典的 /O（第一页）和对象号恢复成页面，/N 是声明的页数，差额有警告。
#[test]
fn pages_are_recovered_when_the_page_tree_root_is_lost() {
    let whole = plain_pdf(&[
        "Page one.",
        "Page two.",
        "Page three.",
        "Page four.",
        "Page five.",
    ]);
    // 去掉页树根（对象 2），当它在丢失的那一半里；前面加一个线性化字典；切在第 4 页的页面对象里
    let root = whole.windows(8).position(|w| w == b"2 0 obj\n").unwrap();
    let end = root
        + whole[root..]
            .windows(6)
            .position(|w| w == b"endobj")
            .unwrap()
        + 7;
    let mut cut = b"%PDF-1.7\n99 0 obj\n<< /Linearized 1 /O 4 /N 5 >>\nendobj\n".to_vec();
    cut.extend_from_slice(&whole[9..root]);
    cut.extend_from_slice(&whole[end..]);
    let stop = cut.windows(9).position(|w| w == b"10 0 obj\n").unwrap() + 20;
    let doc = extract(&cut[..stop]).unwrap();
    assert_eq!(doc.pages.len(), 3, "{:?}", doc.warnings);
    assert_eq!(doc.metadata.page_count, 3);
    let md = &doc.markdown;
    assert!(md.find("Page one.") < md.find("Page two."), "{md}");
    assert!(md.find("Page two.") < md.find("Page three."), "{md}");
    assert!(!md.contains("Page four."));
    assert_eq!(doc.warnings.len(), 2, "{:?}", doc.warnings);
    assert_eq!(
        doc.warnings[1],
        "pages_missing: 2 of 5 pages could not be recovered from the damaged file; 3 converted"
    );
    // 没有线性化字典、也没有 /Pages 节点：不知道该有几页，要明说"后面的页可能丢了"，
    // 不能把找回的 3 页当成整份文件（0.80.3 只报修复过，丢页悄无声息）
    let mut cut = b"%PDF-1.7\n".to_vec();
    cut.extend_from_slice(&whole[9..root]);
    cut.extend_from_slice(&whole[end..]);
    let stop = cut.windows(9).position(|w| w == b"10 0 obj\n").unwrap() + 20;
    let doc = extract(&cut[..stop]).unwrap();
    assert_eq!(doc.pages.len(), 3);
    assert_eq!(doc.warnings.len(), 2, "{:?}", doc.warnings);
    assert_eq!(
        doc.warnings[1],
        "pages_missing: the page tree was lost; 3 pages were recovered by scanning the file, but the total page count is unknown, so later pages may be missing"
    );
}

/// 书签只到章节、/Info 又没有 /Title 的论文（arXiv 常见）：题名不是书签条目，按提示排版时也要
/// 从第一页的版式把它找出来，推断的标题才不是空的。
#[test]
fn document_title_is_inferred_even_when_bookmarks_set_the_headings() {
    let mut c = line("F1", 20.0, 72.0, 740.0, "Attention Is All You Need");
    c.push_str(&line("F1", 10.0, 72.0, 715.0, "Ashish Vaswani"));
    c.push_str(&line("F1", 12.0, 72.0, 690.0, "1 Introduction"));
    let mut y = 670.0;
    for p in [P1, P2] {
        let (s, ny) = paragraph(10.0, 72.0, y, 13.0, 85, p);
        c.push_str(&s);
        y = ny - 12.0;
    }
    let mut second = line("F1", 12.0, 72.0, 740.0, "2 Background");
    second.push_str(&paragraph(10.0, 72.0, 720.0, 13.0, 85, P3).0);
    let mut b = Builder::new(vec![Page::text(c), Page::text(second)]);
    b.outline = vec![("1 Introduction", 0), ("2 Background", 1)];
    let doc = extract(&b.build()).unwrap();
    assert_eq!(doc.metadata.title, None);
    assert!(
        doc.markdown.contains("## 1 Introduction"),
        "{}",
        doc.markdown
    );
    assert!(
        doc.markdown.starts_with("# Attention Is All You Need\n"),
        "{}",
        doc.markdown
    );
    assert_eq!(
        doc.inferred_title.as_deref(),
        Some("Attention Is All You Need")
    );
}

/// /Info 没有标题时，第一页的一级标题作为推断的标题单独给出；有 /Title 时两者都在。
#[test]
fn document_title_is_inferred_from_the_first_heading() {
    let mut c = line("F1", 20.0, 72.0, 740.0, "Attention Is All You Need");
    let mut y = 700.0;
    for p in [P1, P2, P3] {
        let (s, ny) = paragraph(10.0, 72.0, y, 13.0, 85, p);
        c.push_str(&s);
        y = ny - 12.0;
    }
    let doc = extract(&Builder::new(vec![Page::text(c.clone())]).build()).unwrap();
    assert_eq!(doc.metadata.title, None);
    assert_eq!(
        doc.inferred_title.as_deref(),
        Some("Attention Is All You Need")
    );
    assert!(doc.markdown.starts_with("# Attention Is All You Need\n"));
    let mut b = Builder::new(vec![Page::text(c)]);
    b.info = Some(dictionary! { "Title" => Object::string_literal("From Info") });
    let doc = extract(&b.build()).unwrap();
    assert_eq!(doc.metadata.title.as_deref(), Some("From Info"));
    assert_eq!(
        doc.inferred_title.as_deref(),
        Some("Attention Is All You Need")
    );
    // 没有标题行的文档：推断不出来
    let body = paragraph(10.0, 72.0, 700.0, 13.0, 85, P1).0;
    let doc = extract(&Builder::new(vec![Page::text(body)]).build()).unwrap();
    assert_eq!(doc.inferred_title, None);
}

/// 作者名后面上标的星号来自数学字体（CMSY），是脚注引用，不是公式：`Ashish Vaswani*`，
/// 不是 `Ashish $Vaswani^{\ast}$`。正文里真正的公式照旧。
#[test]
fn footnote_markers_after_author_names_are_plain_text() {
    let mut b = Builder::new(Vec::new());
    b.extra_fonts.push((
        "F4".into(),
        dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "CMSY10",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![Object::Integer(20), Object::Name(b"lessequal".to_vec()),
                    Object::Integer(42), Object::Name(b"asteriskmath".to_vec())],
            },
        },
    ));
    let mut c = line("F1", 20.0, 72.0, 740.0, "Attention Is All You Need");
    // 名字 12 pt，星号 8 pt 抬高 3.6 pt，紧跟在名字后面
    c.push_str(&line("F1", 12.0, 72.0, 712.0, "Ashish Vaswani"));
    c.push_str(&line("F4", 8.0, 157.0, 715.6, "*"));
    c.push_str(&line("F1", 12.0, 180.0, 712.0, "Noam Shazeer"));
    c.push_str(&line("F4", 8.0, 261.0, 715.6, "*"));
    let mut y = 680.0;
    for p in [P1, P2, P3] {
        let (s, ny) = paragraph(10.0, 72.0, y, 13.0, 85, p);
        c.push_str(&s);
        y = ny - 12.0;
    }
    let doc = extract(
        &Builder {
            pages: vec![Page::text(c)],
            ..b
        }
        .build(),
    )
    .unwrap();
    assert!(
        doc.markdown.contains("Ashish Vaswani* Noam Shazeer*"),
        "{}",
        doc.markdown
    );
    assert!(!doc.markdown.contains("\\ast"), "{}", doc.markdown);
}

/// 页面字典还在、页数也对，内容流却丢了（arXiv 文件的第一页内容流写在文件后半，截断时就是这样）：
/// 不能悄悄少一页正文，要用 content_missing 指出是哪几页。
#[test]
fn pages_whose_content_stream_was_cut_away_are_named() {
    let whole = plain_pdf(&["Page one.", "Page two.", "Page three."]);
    // 把第一页的内容流（对象 5）挪到文件末尾的 xref 之后，再截掉：页面字典在，内容流没了
    let start = whole.windows(8).position(|w| w == b"5 0 obj\n").unwrap();
    let end = start
        + whole[start..]
            .windows(6)
            .position(|w| w == b"endobj")
            .unwrap()
        + 7;
    let xref = whole.windows(5).position(|w| w == b"xref\n").unwrap();
    let mut cut = whole[..start].to_vec();
    cut.extend_from_slice(&whole[end..xref]);
    let doc = extract(&cut).unwrap();
    assert_eq!(doc.pages.len(), 3);
    assert!(!doc.markdown.contains("Page one.") && doc.markdown.contains("Page three."));
    assert!(
        doc.warnings
            .iter()
            .any(|w| w.starts_with("content_missing: 1 of 3 pages") && w.ends_with("(pages 1)")),
        "{:?}",
        doc.warnings
    );
}

/// 表单域、文本框注释的字在注释的外观流里（不在页面内容里）：照阅读器那样画进来。
/// 没有外观的文本域（/NeedAppearances）取它的值；隐藏的注释和按钮不收。
#[test]
fn annotation_appearances_and_field_values_are_text() {
    let content = line("F1", 14.0, 72.0, 720.0, "Application form");
    let mut doc = Builder::new(vec![Page::text(content)]).document();
    let helv = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1",
        "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
    });
    let mut appearance = |text: &str| {
        doc.add_object(lopdf::Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 150.into(), 20.into()],
                "Resources" => dictionary! { "Font" => dictionary! { "Helv" => helv } },
            },
            format!("BT /Helv 12 Tf 2 5 Td {} Tj ET", lit(text)).into_bytes(),
        ))
    };
    let (name_ap, submit_ap, secret_ap) = (
        appearance("Jane Doe"),
        appearance("Submit"),
        appearance("Secret note"),
    );
    let rect = |y: i64| vec![72.into(), y.into(), 222.into(), (y + 20).into()];
    let annots: Vec<Object> = vec![
        dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx", "F" => 4,
            "Rect" => rect(660), "AP" => dictionary! { "N" => name_ap },
        }
        .into(),
        dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx", "F" => 4,
            "Rect" => rect(620), "DA" => Object::string_literal("/Helv 0 Tf 0 g"),
            "V" => Object::string_literal("Account 12345"),
        }
        .into(),
        dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Btn", "Ff" => 65536, "F" => 4,
            "Rect" => rect(580), "AP" => dictionary! { "N" => submit_ap },
        }
        .into(),
        dictionary! {
            "Type" => "Annot", "Subtype" => "FreeText", "F" => 2,
            "Rect" => rect(540), "AP" => dictionary! { "N" => secret_ap },
        }
        .into(),
    ];
    let page_id = doc.get_pages()[&1];
    doc.get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Annots", annots);
    let md = extract(&save(doc)).unwrap().markdown;
    for want in ["Application form", "Jane Doe", "Account 12345"] {
        assert!(md.contains(want), "{want}: {md}");
    }
    for unwanted in ["Submit", "Secret note"] {
        assert!(!md.contains(unwanted), "{unwanted}: {md}");
    }
}

/// Type3 字体：FontMatrix 是 1:1（不是常见的 1/1000）配 0.12 的字号时，字号按字宽估出的 em 校正，
/// 不当成看不见的小字；字形名是自造的（/BT、/CP）时按基础编码取这个码的字。
#[test]
fn type3_font_with_unit_matrix_and_made_up_glyph_names() {
    let mut b = Builder::new(vec![Page::text(
        "BT /T3 0.12 Tf 1 0 0 -1 72 700 Tm (Type) Tj 60 0 Td (three) Tj ET\n".to_string(),
    )]);
    let glyph = || Object::from(lopdf::Stream::new(dictionary! {}, b"80 0 d0".to_vec()));
    let names = ["BT", "CP", "CR", "CS", "CT", "CW", "CX", "D0"];
    let mut procs = lopdf::Dictionary::new();
    for n in names {
        procs.set(n, glyph());
    }
    b.extra_fonts.push((
        "T3".to_string(),
        dictionary! {
            "Type" => "Font", "Subtype" => "Type3",
            "FontMatrix" => vec![1.into(), 0.into(), 0.into(), (-1).into(), 0.into(), 0.into()],
            "FontBBox" => vec![0.into(), 0.into(), 1.into(), (-1).into()],
            "FirstChar" => 84, "LastChar" => 121,
            "Widths" => (84..=121).map(|_| Object::Integer(70)).collect::<Vec<_>>(),
            "Encoding" => dictionary! {
                "Type" => "Encoding", "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => vec![
                    84.into(), "BT".into(), 101.into(), "CP".into(), 104.into(), "CR".into(),
                    112.into(), "CS".into(), 114.into(), "CT".into(), 116.into(), "CW".into(),
                    121.into(), "CX".into(),
                ],
            },
            "CharProcs" => procs,
            "Resources" => dictionary! {},
        },
    ));
    let md = extract(&b.build()).unwrap().markdown;
    assert!(md.contains("Type three"), "{md}");
}
