//! 基于 proptest 的模糊测试：自研的词法器、CMap、解释器和版面重组在任意输入上都不能 panic，
//! 也不能卡住。更长时间的覆盖率引导模糊测试见 `fuzz/` 目录（cargo-fuzz）。

mod common;

use std::time::Duration;

use common::{Builder, Page, line, paragraph, save_raw};
use pagesift_pdf::Options;
use pagesift_pdf::fuzzing;
use pagesift_testkit::cpu_time;
use proptest::prelude::*;

const CONTENT_TOKENS: &[&str] = &[
    "BT",
    "ET",
    "Tf",
    "Td",
    "TD",
    "Tm",
    "T*",
    "Tj",
    "TJ",
    "'",
    "\"",
    "Tc",
    "Tw",
    "Tz",
    "TL",
    "Ts",
    "q",
    "Q",
    "cm",
    "Do",
    "BI",
    "ID",
    "EI",
    "/F1",
    "/F3",
    "/Im1",
    "/Nope",
    "(",
    ")",
    "(abc)",
    "(\\",
    "\\(",
    "<",
    ">",
    "<<",
    ">>",
    "<4E2D>",
    "<0",
    "[",
    "]",
    "{",
    "}",
    "%",
    "\n",
    " ",
    "0",
    "1",
    "-1",
    "12",
    "1e9",
    "-0.000001",
    "99999999999",
    ".5",
    "--3",
    "+",
    "#",
];

const CMAP_TOKENS: &[&str] = &[
    "begincodespacerange",
    "endcodespacerange",
    "beginbfchar",
    "endbfchar",
    "beginbfrange",
    "endbfrange",
    "begincidrange",
    "endcidrange",
    "begincidchar",
    "endcidchar",
    "usecmap",
    "<00>",
    "<FF>",
    "<0000>",
    "<FFFF>",
    "<FFFFFFFF>",
    "<00000000>",
    "<D83D>",
    "<DE00>",
    "<4E2D>",
    "<>",
    "<0",
    "[",
    "]",
    "[<0041>",
    "/space",
    "/uni4E2D",
    "/Identity-H",
    "1",
    "0",
    "-5",
    "65535",
    "4294967295",
    " ",
    "\n",
];

fn soup(tokens: &'static [&'static str], max: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop::sample::select(tokens), 0..max)
        .prop_map(|v| v.join(" ").into_bytes())
}

fn sample_pdf() -> Vec<u8> {
    let mut c = line("F2", 18.0, 72.0, 720.0, "Fuzz Sample");
    c.push_str(
        &paragraph(
            11.0,
            72.0,
            690.0,
            14.0,
            60,
            "Some text that wraps over a few lines so that the layout code has work to do.",
        )
        .0,
    );
    let mut b = Builder::new(vec![
        Page::text(c.clone()),
        Page {
            content: c,
            image: true,
        },
    ]);
    let mut cjk = common::CjkFont::default();
    b.pages[0].content.push_str(&format!(
        "BT /F3 12 Tf 72 500 Td {} Tj ET\n",
        cjk.hex("模糊测试")
    ));
    b.cjk = Some(cjk);
    save_raw(b.document())
}

fn quick() -> Options {
    Options {
        timeout: Some(Duration::from_secs(5)),
        ..Options::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig::default())]

    #[test]
    fn lexer_on_random_bytes(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        fuzzing::lex(&data);
    }

    #[test]
    fn lexer_on_token_soup(data in soup(CONTENT_TOKENS, 400)) {
        fuzzing::lex(&data);
    }

    #[test]
    fn cmap_on_token_soup(data in soup(CMAP_TOKENS, 300), probe in prop::collection::vec(any::<u8>(), 0..6)) {
        fuzzing::cmap(&data, &probe);
    }

    #[test]
    fn layout_on_random_glyphs(pages in prop::collection::vec(
        prop::collection::vec(
            ("[a-zA-Z#•中 -]{0,3}", 0.0..612.0f64, 0.0..792.0f64, 0.0..30.0f64, 0.2..40.0f64, any::<bool>()),
            0..120,
        ),
        0..4,
    )) {
        fuzzing::layout(&pages);
    }
}

/// 构造整份 PDF 的用例比较慢，默认少跑一些；本地压测可设 PROPTEST_CASES。
fn heavy_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(heavy_cases()))]

    #[test]
    fn interpreter_on_content_soup(content in soup(CONTENT_TOKENS, 600)) {
        let pdf = Builder::new(vec![Page {
            content: String::from_utf8_lossy(&content).into_owned(),
            image: false,
        }])
        .build();
        let (r, took) = cpu_time(|| fuzzing::extract_unguarded(&pdf, &quick()));
        prop_assert!(r.is_ok());
        prop_assert!(took < Duration::from_secs(5), "{:?}", took);
    }

    #[test]
    fn mutated_pdf(flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..16),
                   cut in any::<prop::sample::Index>(), truncate in any::<bool>()) {
        let mut pdf = sample_pdf();
        for (i, b) in flips {
            let i = i.index(pdf.len());
            pdf[i] = b;
        }
        if truncate {
            pdf.truncate(cut.index(pdf.len()));
        }
        let (_, took) = cpu_time(|| fuzzing::extract_unguarded(&pdf, &quick()));
        prop_assert!(took < Duration::from_secs(5), "{:?}", took);
    }
}
