use std::fs;
use std::path::Path;

use pagesift_office::{Error, Format, FormatExt, Options, convert, is_fetchable};

fn tar_single_file(name: &str, content: &[u8]) -> Vec<u8> {
    let mut blk = vec![0u8; 512];
    let name_bytes = name.as_bytes();
    blk[..name_bytes.len()].copy_from_slice(name_bytes);
    blk[100..108].copy_from_slice(b"0000644\0");
    blk[108..116].copy_from_slice(b"0000000\0");
    blk[116..124].copy_from_slice(b"0000000\0");
    let size_oct = format!("{:011o}\0", content.len());
    blk[124..136].copy_from_slice(size_oct.as_bytes());
    blk[136..148].copy_from_slice(b"00000000000\0");
    blk[148..156].copy_from_slice(b"        ");
    blk[156] = b'0';
    blk[257..263].copy_from_slice(b"ustar\0");
    blk[263..265].copy_from_slice(b"00");
    let sum: u32 = blk.iter().map(|&b| b as u32).sum();
    let chk = format!("{sum:06o}\0 ");
    blk[148..156].copy_from_slice(chk.as_bytes());

    let mut out = blk;
    out.extend_from_slice(content);
    let rem = content.len() % 512;
    if rem != 0 {
        out.resize(out.len() + (512 - rem), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

#[test]
fn fetchable_policy_matches_pagesift_contract() {
    let fetchable = [
        Format::Docx,
        Format::Xlsx,
        Format::Pptx,
        Format::Epub,
        Format::Csv,
        Format::Rtf,
        Format::Odt,
        Format::Ods,
        Format::Odp,
        Format::Eml,
        Format::Mbox,
        Format::Msg,
        Format::Archive,
        Format::Doc,
        Format::Xls,
        Format::Ppt,
        Format::Xlsb,
        Format::Hwp,
        Format::Hwpx,
        Format::Sqlite,
        Format::Mobi,
    ];
    for fmt in fetchable {
        assert!(fmt.fetchable(), "{fmt:?} must be fetchable");
        assert!(is_fetchable(fmt));
    }

    let non_fetchable = [
        Format::Text,
        Format::Markdown,
        Format::Fenced("json"),
        Format::Subtitle,
        Format::Notebook,
        Format::Outline,
        Format::Fb2,
        Format::Docbook,
        Format::Jats,
        Format::Svg,
        Format::WordPerfect,
        Format::FlatOdf,
        Format::Dbf,
    ];
    for fmt in non_fetchable {
        assert!(!fmt.fetchable(), "{fmt:?} must not be fetchable");
        assert!(!is_fetchable(fmt));
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_archive_is_detected_and_converted() {
    let tar = tar_single_file("hello.txt", b"Hello from tar.zst!\n");
    let zst = pagesift_compress::compress(&tar, 3);

    let fmt = Format::detect(&zst, Some("bundle.tar.zst"), Some("application/zstd"));
    assert_eq!(fmt, Some(Format::Archive));
    assert!(fmt.unwrap().fetchable());

    let member = |bytes: &[u8],
                  name: &str,
                  _budget: usize|
     -> Result<Option<pagesift_office::Converted>, Error> {
        let Some(f) = Format::detect(bytes, Some(name), None) else {
            return Ok(None);
        };
        convert(bytes, f, &Options::default()).map(Some)
    };
    let opts = Options {
        member: Some(&member),
        ..Options::default()
    };
    let out = convert(&zst, Format::Archive, &opts).unwrap();
    assert!(out.markdown.contains("Hello from tar.zst!"));
}

#[cfg(not(feature = "zstd"))]
#[test]
fn zstd_archive_requires_zstd_feature() {
    let tar = tar_single_file("hello.txt", b"Hello from tar.zst!\n");
    let zst = pagesift_compress::compress(&tar, 3);

    let fmt = Format::detect(&zst, Some("bundle.tar.zst"), Some("application/zstd"));
    assert_eq!(fmt, None);
    let err = convert(&zst, Format::Archive, &Options::default()).unwrap_err();
    assert!(
        !matches!(err, Error::Timeout),
        "unexpected timeout: {err:?}"
    );
}

#[test]
fn encrypted_ole_and_compress_fixtures_behave_cleanly() {
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(512, 0);
    assert_eq!(
        convert(&ole, Format::Docx, &Options::default()),
        Err(Error::Encrypted)
    );

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/compress");
    for name in ["text.bz2", "text.xz", "two-streams.xz", "files.7z"] {
        let path = dir.join(name);
        let bytes = fs::read(&path).expect("fixture readable");
        let fmt = Format::detect(&bytes, Some(name), None);
        assert_eq!(
            fmt,
            Some(Format::Archive),
            "{name} should detect as Archive"
        );
        let out = convert(&bytes, Format::Archive, &Options::default()).unwrap();
        assert!(!out.markdown.is_empty(), "{name} should yield output");
    }
}
