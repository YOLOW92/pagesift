//! EPUB 2/3（OCF 3.3、Packages 3.3）：按 OPF 的 spine 顺序把每个 XHTML 内容文档交给调用方的
//! HTML 转换器，拼成一份 Markdown。标题、作者等取自 OPF 的 Dublin Core 元数据。
//! 内容文档被加密（DRM）时报 [`Error::Encrypted`]；字体混淆（IDPF/Adobe）不算。

use crate::opc::resolve;
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Options, check_deadline};

const FONT_OBFUSCATION: [&str; 2] = [
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    let html = opts
        .html
        .ok_or_else(|| Error::Unsupported("EPUB needs an HTML converter (Options::html)".into()))?;
    let container = zip
        .read_text("META-INF/container.xml")?
        .ok_or_else(|| Error::Malformed("EPUB without META-INF/container.xml".into()))?;
    let opf_path = rootfile(&container)?;
    if let Some(enc) = zip.read_text("META-INF/encryption.xml")?
        && drm(&enc)?
    {
        return Err(Error::Encrypted);
    }
    let opf = zip
        .read_text(&opf_path)?
        .ok_or_else(|| Error::Malformed(format!("package document {opf_path} is missing")))?;
    let base = opf_path.rsplit_once('/').map_or("", |(d, _)| d);
    let pkg = package(&opf)?;

    let mut out = Converted::default();
    for (k, v) in pkg.meta {
        if k == "title" && out.title.is_none() {
            out.title = Some(v);
        } else {
            out.metadata.push((k, v));
        }
    }
    let mut parts = Vec::new();
    for idref in &pkg.spine {
        let Some((href, media)) = pkg
            .manifest
            .iter()
            .find(|(id, _, _)| id == idref)
            .map(|(_, h, m)| (h, m))
        else {
            continue;
        };
        let xhtml = media.contains("html")
            || href.ends_with(".html")
            || href.ends_with(".htm")
            || href.ends_with(".xhtml");
        if !xhtml {
            continue; // spine 里的 SVG、图片等
        }
        check_deadline(opts)?;
        let path = resolve(base, href);
        let Some(doc) = zip.read_text(&path)? else {
            out.warnings.push(format!("spine item {path} is missing"));
            continue;
        };
        let md = html(&doc, &path);
        if !md.trim().is_empty() {
            parts.push(md.trim().to_string());
        }
    }
    out.parts = parts.len();
    out.markdown = parts.join("\n\n");
    out.markdown.push('\n');
    Ok(out)
}

fn rootfile(xml: &str) -> Result<String, Error> {
    let mut r = Reader::new(xml);
    let mut first = None;
    while let Some(t) = r.next()? {
        if let Tok::Start(name, a) = t
            && local_name(name) == "rootfile"
            && let Some(p) = a.get("full-path")
        {
            if a.get("media-type")
                .is_none_or(|m| m == "application/oebps-package+xml")
            {
                return Ok(p.into_owned());
            }
            first.get_or_insert(p.into_owned());
        }
    }
    first.ok_or_else(|| Error::Malformed("container.xml names no rootfile".into()))
}

/// encryption.xml 里有没有用非字体混淆算法加密的内容文档。
fn drm(xml: &str) -> Result<bool, Error> {
    let mut r = Reader::new(xml);
    let mut algorithm = String::new();
    while let Some(t) = r.next()? {
        let Tok::Start(name, a) = t else { continue };
        match local_name(name) {
            "EncryptionMethod" => algorithm = a.get("Algorithm").unwrap_or_default().into_owned(),
            "CipherReference" => {
                let uri = a.get("URI").unwrap_or_default().to_ascii_lowercase();
                let content = [".xhtml", ".html", ".htm", ".opf", ".ncx"]
                    .iter()
                    .any(|e| uri.ends_with(e));
                if content && !FONT_OBFUSCATION.contains(&algorithm.as_str()) {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

struct Package {
    meta: Vec<(String, String)>,
    /// (id, href, media-type)
    manifest: Vec<(String, String, String)>,
    spine: Vec<String>,
}

fn package(xml: &str) -> Result<Package, Error> {
    let mut pkg = Package {
        meta: Vec::new(),
        manifest: Vec::new(),
        spine: Vec::new(),
    };
    let mut r = Reader::new(xml);
    let mut in_meta = false;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "metadata" => in_meta = true,
                k @ ("title" | "creator" | "language" | "publisher" | "date" | "description"
                | "subject" | "identifier")
                    if in_meta =>
                {
                    let v = crate::md::squeeze(&r.text_until_end()?);
                    if !v.is_empty() {
                        pkg.meta.push((k.to_string(), v));
                    }
                }
                "item" => {
                    if let (Some(id), Some(href)) = (a.get("id"), a.get("href")) {
                        let media = a.get("media-type").unwrap_or_default().into_owned();
                        pkg.manifest
                            .push((id.into_owned(), href.into_owned(), media));
                    }
                }
                "itemref" => {
                    if let Some(id) = a.get("idref") {
                        pkg.spine.push(id.into_owned());
                    }
                }
                _ => {}
            },
            Tok::End(name) if local_name(name) == "metadata" => in_meta = false,
            _ => {}
        }
    }
    Ok(pkg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::build;

    fn epub(extra: &[(&str, &str)]) -> Vec<u8> {
        let container = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
        let opf = r#"<package><metadata xmlns:dc="dc"><dc:title>My Book</dc:title><dc:creator>Ann</dc:creator></metadata>
<manifest><item id="c2" href="text/ch%202.xhtml" media-type="application/xhtml+xml"/><item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/><item id="img" href="i.png" media-type="image/png"/></manifest>
<spine><itemref idref="c1"/><itemref idref="img"/><itemref idref="c2"/></spine></package>"#;
        let mut parts: Vec<(&str, &[u8], bool)> = vec![
            ("mimetype", b"application/epub+zip", false),
            ("META-INF/container.xml", container.as_bytes(), true),
            ("OEBPS/content.opf", opf.as_bytes(), true),
            ("OEBPS/text/ch1.xhtml", b"<h1>One</h1>", true),
            ("OEBPS/text/ch 2.xhtml", b"<h1>Two</h1>", true),
        ];
        for (n, d) in extra {
            parts.push((n, d.as_bytes(), true));
        }
        build(&parts)
    }

    fn to_md(html: &str, path: &str) -> String {
        format!(
            "{} ({path})",
            html.replace("<h1>", "# ").replace("</h1>", "")
        )
    }

    #[test]
    fn follows_the_spine_and_reads_metadata() {
        let f = to_md;
        let opts = Options {
            html: Some(&f),
            ..Options::default()
        };
        let got = crate::convert(&epub(&[]), crate::Format::Epub, &opts).unwrap();
        assert_eq!(
            got.markdown,
            "# One (OEBPS/text/ch1.xhtml)\n\n# Two (OEBPS/text/ch 2.xhtml)\n"
        );
        assert_eq!(got.title.as_deref(), Some("My Book"));
        assert_eq!(
            got.metadata,
            vec![("creator".to_string(), "Ann".to_string())]
        );
        assert_eq!(got.parts, 2);
    }

    #[test]
    fn drm_is_encrypted_but_font_obfuscation_is_fine() {
        let f = to_md;
        let opts = Options {
            html: Some(&f),
            ..Options::default()
        };
        let drm = r#"<encryption><EncryptedData><EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/><CipherData><CipherReference URI="OEBPS/text/ch1.xhtml"/></CipherData></EncryptedData></encryption>"#;
        assert_eq!(
            crate::convert(
                &epub(&[("META-INF/encryption.xml", drm)]),
                crate::Format::Epub,
                &opts
            ),
            Err(Error::Encrypted)
        );
        let fonts = r#"<encryption><EncryptedData><EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/><CipherData><CipherReference URI="OEBPS/font.otf"/></CipherData></EncryptedData></encryption>"#;
        assert!(
            crate::convert(
                &epub(&[("META-INF/encryption.xml", fonts)]),
                crate::Format::Epub,
                &opts
            )
            .is_ok()
        );
        assert!(matches!(
            crate::convert(&epub(&[]), crate::Format::Epub, &Options::default()),
            Err(Error::Unsupported(_))
        ));
    }
}
