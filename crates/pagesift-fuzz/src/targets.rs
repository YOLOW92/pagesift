//! 被测目标：每个目标是“字节 → 调用一组公开入口”的函数，外加一份变异用的字典。

use std::time::Duration;

pub struct Target {
    pub name: &'static str,
    pub run: fn(&[u8], u64),
    pub dict: &'static [&'static [u8]],
    pub max_len: usize,
    /// 单个输入超过这个时间算卡死。
    pub timeout: Duration,
}

pub const TARGETS: &[Target] = &[
    Target {
        name: "html",
        run: html,
        dict: HTML_DICT,
        max_len: 2 << 20,
        timeout: Duration::from_secs(2),
    },
    Target {
        name: "markdown",
        run: markdown,
        dict: HTML_DICT,
        max_len: 2 << 20,
        timeout: Duration::from_secs(5),
    },
    Target {
        name: "charset",
        run: charset,
        dict: CHARSET_DICT,
        max_len: 1 << 20,
        timeout: Duration::from_secs(2),
    },
    Target {
        name: "pdf",
        run: pdf,
        dict: PDF_DICT,
        max_len: 16 << 20,
        // 解析器自己的超时是 10 秒，看门狗留出余量
        timeout: Duration::from_secs(20),
    },
    Target {
        name: "jpeg",
        run: jpeg,
        dict: JPEG_DICT,
        max_len: 1 << 20,
        timeout: Duration::from_secs(5),
    },
    Target {
        name: "ccitt",
        run: ccitt,
        dict: CCITT_DICT,
        max_len: 1 << 20,
        timeout: Duration::from_secs(5),
    },
    Target {
        name: "jbig2",
        run: jbig2,
        dict: JBIG2_DICT,
        max_len: 1 << 20,
        timeout: Duration::from_secs(5),
    },
];

/// JBIG2 段头的常见片段（T.88 §7.2、表 2）：各段类型的标志字节配上短格式引用和 1 字节页关联、
/// 文件头 ID、MQ 码流里的标记、页尺寸未知。
const JBIG2_DICT: &[&[u8]] = &[
    b"\x00\x00\x00\x00\x30\x00\x01",
    b"\x00\x00\x00\x01\x00\x00\x01",
    b"\x00\x00\x00\x02\x06\x20\x01",
    b"\x00\x00\x00\x02\x26\x00\x01",
    b"\x00\x00\x00\x02\x27\x00\x01",
    b"\x00\x00\x00\x02\x2a\x00\x01",
    b"\x00\x00\x00\x02\x10\x00\x01",
    b"\x00\x00\x00\x02\x16\x20\x01",
    b"\x00\x00\x00\x02\x35\x00\x01",
    b"\x00\x00\x00\x03\x32\x00\x01",
    b"\x97JB2\r\n\x1a\n",
    b"\xff\xac",
    b"\xff\xff\xff\xff",
    b"\x00\x00\x00\x00",
];

/// CCITT 码（T.4 表 2–4）：EOL、EOFB、各二维模式码、长的组合码，按字节对齐放。
const CCITT_DICT: &[&[u8]] = &[
    b"\x00\x10",
    b"\x00\x10\x01",
    b"\x00\x00",
    b"\x80",
    b"\x20",
    b"\x10",
    b"\x60",
    b"\x40",
    b"\x0c",
    b"\x08",
    b"\x06",
    b"\x04",
    b"\x02",
    b"\x01\xf0",
    b"\xff",
];

/// JPEG 标记（T.81 表 B.1）：SOI、SOF0/1/2、DHT、DQT、DRI、SOS、RST0、EOI、APP14、填充。
const JPEG_DICT: &[&[u8]] = &[
    b"\xff\xd8",
    b"\xff\xc0",
    b"\xff\xc1",
    b"\xff\xc2",
    b"\xff\xc4",
    b"\xff\xdb",
    b"\xff\xdd",
    b"\xff\xda",
    b"\xff\xd0",
    b"\xff\xd9",
    b"\xff\xeeAdobe",
    b"\xff\x00",
];

const HTML_DICT: &[&[u8]] = &[
    b"<p>",
    b"</p>",
    b"<div>",
    b"</div>",
    b"<b>",
    b"</b>",
    b"<a href=x>",
    b"</a>",
    b"<i>",
    b"<table>",
    b"</table>",
    b"<tr>",
    b"<td>",
    b"</td>",
    b"<th>",
    b"<tbody>",
    b"<caption>",
    b"<colgroup>",
    b"<col>",
    b"<select>",
    b"<option>",
    b"<optgroup>",
    b"<li>",
    b"<ul>",
    b"<ol>",
    b"<dl>",
    b"<dt>",
    b"<dd>",
    b"<svg>",
    b"</svg>",
    b"<math>",
    b"<mi>",
    b"<foreignObject>",
    b"<annotation-xml encoding=\"text/html\">",
    b"<desc>",
    b"<template>",
    b"</template>",
    b"<frameset>",
    b"<frame>",
    b"<noscript>",
    b"<script>",
    b"</script>",
    b"<style>",
    b"<textarea>",
    b"<title>",
    b"<xmp>",
    b"<plaintext>",
    b"<pre>",
    b"<listing>",
    b"<iframe>",
    b"<form>",
    b"</form>",
    b"<button>",
    b"<h1>",
    b"</h2>",
    b"<nobr>",
    b"<font color=red>",
    b"<html>",
    b"<head>",
    b"<body>",
    b"</body>",
    b"</html>",
    b"<!DOCTYPE html>",
    b"<!--",
    b"-->",
    b"--!>",
    b"<![CDATA[",
    b"]]>",
    b"<?xml ?>",
    b"</",
    b"<",
    b">",
    b"/>",
    b"&amp;",
    b"&#x1F600;",
    b"&#0;",
    b"&#xD800;",
    b"&notin;",
    b"&not",
    b"&",
    b"=\"",
    b"'",
    b"\"",
    b" a=b",
    b" class=\"x y\"",
    b" id=main",
    b" style=\"display:none\"",
    b"<img src=x>",
    b"<br>",
    b"<hr>",
    b"<meta charset=utf-8>",
    b"<base href=\"http://a/b/\">",
    b"<article>",
    b"<main>",
    b"<nav>",
    b"<header>",
    b"<footer>",
    b"<ruby>",
    b"<rt>",
    b"<rp>",
    b"<code>",
    b"<object>",
    b"<applet>",
    b"<marquee>",
    b"<image>",
    b"<sarcasm>",
    b"<math><mtext>",
    b"\xef\xbb\xbf",
    b"\x00",
    b"\r\n",
    b"\t",
    b"\xc3\xa9",
    b"\xe4\xb8\xad",
    b"\xff",
];

const CHARSET_DICT: &[&[u8]] = &[
    b"<meta charset=\"shift_jis\">",
    b"<meta charset=utf-8>",
    b"<meta charset=gbk>",
    b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=euc-kr\">",
    b"<meta charset=x-user-defined>",
    b"<meta charset=utf-16le>",
    b"<meta charset=iso-2022-jp>",
    b"<meta charset=windows-1251>",
    b"<meta charset=big5>",
    b"<meta charset=\"",
    b"<?xml version=\"1.0\" encoding=\"euc-jp\"?>",
    b"\xef\xbb\xbf",
    b"\xff\xfe",
    b"\xfe\xff",
    b"\x1b$B",
    b"\x1b(B",
    b"\x1b$)C",
    b"\x0e",
    b"\x0f",
    b"\x82\xa0",
    b"\xa4\xa2",
    b"\xc4\xe3",
    b"\xb0\xa1",
    b"\xe3\x81\x82",
    b"\xd0\xbf",
    b"\xf0\x9f\x98\x80",
    b"\xc0\x80",
    b"\xed\xa0\x80",
    b"+ZeVnLIqe-",
    b"<!--",
    b"-->",
    b"<",
    b">",
    b"\x00",
];

const PDF_DICT: &[&[u8]] = &[
    b"%PDF-1.7\n",
    b"obj",
    b"endobj",
    b"stream\n",
    b"\nendstream",
    b"xref\n",
    b"trailer",
    b"startxref\n",
    b"%%EOF",
    b"<<",
    b">>",
    b"[",
    b"]",
    b"(",
    b")",
    b"<",
    b">",
    b"/Type",
    b"/Page",
    b"/Pages",
    b"/Kids",
    b"/Count 999999999",
    b"/Length 0",
    b"/Length 99999999",
    b"/Filter /FlateDecode",
    b"/Filter [/ASCIIHexDecode /FlateDecode]",
    b"/DecodeParms",
    b"/Predictor 12",
    b"/Columns 4",
    b"/ObjStm",
    b"/N 3",
    b"/First 0",
    b"/Prev 0",
    b"/XRefStm",
    b"/W [1 2 1]",
    b"/Index [0 5]",
    b"/Root 1 0 R",
    b"1 0 R",
    b"0 0 R",
    b"/Font",
    b"/ToUnicode",
    b"/Encoding",
    b"/Differences [1 /a]",
    b"/Contents",
    b"/Resources",
    b"/MediaBox [0 0 612 792]",
    b"BT",
    b"ET",
    b"Tf",
    b"Tj",
    b"TJ",
    b"Td",
    b"Tm",
    b"cm",
    b"q",
    b"Q",
    b"Do",
    b"BI",
    b"ID",
    b"EI",
    b"beginbfchar",
    b"endbfchar",
    b"beginbfrange",
    b"endbfrange",
    b"begincodespacerange",
    b"/Outlines",
    b"/First 1 0 R",
    b"/Next 1 0 R",
    b"/StructTreeRoot",
    b"/K [",
    b"/Encrypt",
    b"\\",
    b"\\(",
    b"\\377",
    b"#20",
    b"-0.0",
    b"1e308",
    b"99999999999999999999",
];

fn html(data: &[u8], iter: u64) {
    let text = String::from_utf8_lossy(data);
    let doc = pagesift_html::Html::parse(&text);
    let root = doc.root();
    let mut n = 0usize;
    for node in root.descendants() {
        if let Some(t) = node.as_text() {
            n += t.len();
        }
        if let Some(el) = node.as_element() {
            n += el.attrs().count();
        }
    }
    std::hint::black_box(n);
    std::hint::black_box(root.text());
    std::hint::black_box(root.html());
    std::hint::black_box(doc.body().text());
    for sel in [
        "p",
        "div > a[href]",
        "table td:first-child",
        "ul li:nth-child(2n+1)",
        "*:not(script)",
    ] {
        let s = pagesift_html::Selector::parse(sel).expect("固定选择器");
        std::hint::black_box(root.select(&s).count());
    }
    // 短输入顺便当选择器解析
    if text.len() < 512
        && iter.is_multiple_of(2)
        && let Ok(s) = pagesift_html::Selector::parse(&text)
    {
        std::hint::black_box(root.select(&s).count());
    }
}

fn markdown(data: &[u8], iter: u64) {
    let text = String::from_utf8_lossy(data);
    let opts = pagesift_content::Options {
        base_url: url_for(iter),
        only_main_content: !iter.is_multiple_of(3),
        plain_text: true,
        clean_html: iter.is_multiple_of(2),
        links: true,
        images: true,
        fingerprints: iter.is_multiple_of(4),
        ..Default::default()
    };
    let doc = pagesift_content::process(&text, &opts);
    std::hint::black_box(doc.markdown.len());
}

fn url_for(iter: u64) -> Option<url::Url> {
    (!iter.is_multiple_of(5))
        .then(|| url::Url::parse("https://example.com/a/b/page.html?q=1").unwrap())
}

fn charset(data: &[u8], iter: u64) {
    const CT: &[Option<&str>] = &[
        None,
        Some("text/html"),
        Some("text/html; charset=utf-8"),
        Some("text/html; charset=\"shift_jis\""),
        Some("text/html;charset=gb2312"),
        Some("text/html; charset=bogus-xx"),
        Some("application/xhtml+xml; charset=utf-16"),
    ];
    const TLD: &[Option<&[u8]>] = &[
        None,
        Some(b"jp"),
        Some(b"ru"),
        Some(b"cn"),
        Some(b"kr"),
        Some(b"tw"),
        Some(b"tr"),
    ];
    let ct = CT[(iter % CT.len() as u64) as usize];
    let tld = TLD[((iter / 7) % TLD.len() as u64) as usize];
    std::hint::black_box(pagesift_net::decode(data, ct, tld));
    std::hint::black_box(pagesift_net::detect_charset(data, ct, tld));
}

fn pdf(data: &[u8], _iter: u64) {
    let opts = pagesift_pdf::Options {
        timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    let _ = std::hint::black_box(pagesift_pdf::fuzzing::extract_unguarded(data, &opts));
}

fn ccitt(data: &[u8], iter: u64) {
    // 码流不带参数：行宽轮流取种子里的宽度（testdata 的三张、B5 常见宽度、/Columns 缺省 1728）
    // 和几个边界值，/K 轮流取三种编码方式，偶尔字节对齐、给定行数或 BlackIs1
    const COLS: &[u32] = &[1211, 2200, 32, 2763, 1013, 1728, 1, 7, 65, 4000];
    let n = COLS.len() as u64;
    let cols = COLS[(iter % n) as usize];
    let k = [-1, 0, 1][((iter / n) % 3) as usize];
    let rows = if (iter / 7).is_multiple_of(2) { 0 } else { 33 };
    let align = (iter / 11).is_multiple_of(4);
    let black1 = (iter / 13).is_multiple_of(5);
    let _ = std::hint::black_box(pagesift_pdf::fuzzing::decode_ccitt(
        data, k, cols, rows, black1, align,
    ));
}

fn jbig2(data: &[u8], iter: u64) {
    // 偶尔把输入切成全局段和页段两份（PDF 的 /JBIG2Globals）
    let (g, page) = if iter.is_multiple_of(4) && data.len() > 1 {
        let at = (iter / 4) as usize % data.len();
        (Some(&data[..at]), &data[at..])
    } else {
        (None, data)
    };
    let _ = std::hint::black_box(pagesift_pdf::fuzzing::decode_jbig2(page, g));
}

fn jpeg(data: &[u8], iter: u64) {
    // 轮流试 /ColorTransform 的三种取值和灰度输出
    let ct = [None, Some(false), Some(true)][(iter % 3) as usize];
    let _ = std::hint::black_box(pagesift_pdf::fuzzing::decode_jpeg(
        data,
        ct,
        iter.is_multiple_of(2),
    ));
}
