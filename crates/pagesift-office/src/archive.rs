//! 压缩包（ZIP、tar、7z，以及 gz、bz2、xz 压缩的单个文件或 tar）：先列清单，再按路径顺序逐份输出内容。内层文件的转换交给调用方
//! （[`crate::Options::member`]），这样 PDF、HTML、嵌套压缩包都走和顶层相同的代码。
//!
//! 炸弹防护：嵌套层数、文件总数、累计解压字节数都是整次转换共享的（线程局部，内层压缩包接着用同一份额度），
//! 再加上输出预算和请求的截止时间。超限时已经转出的部分照常返回，并写明警告。

use std::cell::Cell;

use crate::compress::{bzip2, sevenz, xz};
use crate::md::{self, Block};
use crate::{Converted, Error, Options, budget, check_deadline, tar, zip};

/// 嵌套层数上限（最外层算 1）：压缩包里的压缩包到第三层为止。
pub(crate) const MAX_DEPTH: usize = 3;
/// 一次转换最多处理这么多个文件（含内层）。
const MAX_MEMBERS: usize = 1_000;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    static MEMBERS_LEFT: Cell<usize> = const { Cell::new(0) };
    static BYTES_LEFT: Cell<u64> = const { Cell::new(0) };
}

/// 进入一层嵌套；最外层负责设置额度，退出时恢复。
pub(crate) struct Nest;

impl Nest {
    pub(crate) fn enter(opts: &Options<'_>) -> Option<Nest> {
        let depth = DEPTH.get();
        if depth >= MAX_DEPTH {
            return None;
        }
        if depth == 0 {
            MEMBERS_LEFT.set(MAX_MEMBERS);
            BYTES_LEFT.set(opts.limits.max_total_bytes);
        }
        DEPTH.set(depth + 1);
        Some(Nest)
    }
}

impl Drop for Nest {
    fn drop(&mut self) {
        DEPTH.set(DEPTH.get().saturating_sub(1));
    }
}

/// 现在已经在第几层。
#[cfg(test)]
pub(crate) fn depth() -> usize {
    DEPTH.get()
}

/// 用掉一个文件名额；用完了返回 false。邮件附件和压缩包成员共用。
pub(crate) fn take_member() -> bool {
    let left = MEMBERS_LEFT.get();
    if left == 0 {
        return false;
    }
    MEMBERS_LEFT.set(left - 1);
    true
}

/// 一个待处理的文件：字节要么借自外层，要么刚解压出来。
struct Member<'a> {
    path: String,
    size: u64,
    read: Source<'a>,
}

enum Source<'a> {
    Borrowed(&'a [u8]),
    Zip(&'a zip::Zip<'a>),
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let Some(_nest) = Nest::enter(opts) else {
        return Err(Error::TooLarge("archive nesting"));
    };
    let limit = usize::try_from(BYTES_LEFT.get()).unwrap_or(usize::MAX);
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let (data, name) = tar::gunzip(bytes, limit)?;
        return from_compressed(&data, name, opts);
    }
    if is_bzip2(bytes) {
        return from_compressed(&bzip2::decode(bytes, limit)?, None, opts);
    }
    if bytes.starts_with(&xz::MAGIC) {
        return from_compressed(&xz::decode(bytes, limit)?, None, opts);
    }
    if bytes.starts_with(&sevenz::MAGIC) {
        let files = sevenz::decode(bytes, limit)?;
        let total: u64 = files.iter().map(|(_, d)| d.len() as u64).sum();
        BYTES_LEFT.set(BYTES_LEFT.get().saturating_sub(total));
        let members = files
            .iter()
            .map(|(path, data)| Member {
                path: path.clone(),
                size: data.len() as u64,
                read: Source::Borrowed(data),
            })
            .collect();
        return render(members, Vec::new(), opts);
    }
    if bytes.starts_with(b"PK") {
        let z = zip::Zip::open(bytes, &opts.limits)?;
        let members = z
            .names()
            .filter(|(name, _)| !name.ends_with('/'))
            .map(|(name, size)| Member {
                path: name.to_owned(),
                size,
                read: Source::Zip(&z),
            })
            .collect();
        return render(members, Vec::new(), opts);
    }
    if tar::looks_like_tar(bytes) {
        return from_tar(bytes, opts);
    }
    Err(Error::Malformed("not an archive".into()))
}

/// 文件开头能确定是压缩格式。bzip2 的 `BZh` 太短，还要看后面的块标记。
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
        || is_bzip2(bytes)
        || bytes.starts_with(&xz::MAGIC)
        || bytes.starts_with(&sevenz::MAGIC)
}

fn is_bzip2(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BZh")
        && bytes.get(4..10).is_some_and(|m| {
            m == [0x31, 0x41, 0x59, 0x26, 0x53, 0x59] || m == [0x17, 0x72, 0x45, 0x38, 0x50, 0x90]
        })
}

/// gz、bz2、xz 解开后：是 tar 就按 tar，否则是单个文件。原名不知道时按内容给一个名字，
/// 这样文字内容还能被认出来。
fn from_compressed(
    data: &[u8],
    name: Option<String>,
    opts: &Options<'_>,
) -> Result<Converted, Error> {
    BYTES_LEFT.set(BYTES_LEFT.get().saturating_sub(data.len() as u64));
    if tar::looks_like_tar(data) {
        return from_tar(data, opts);
    }
    let textual = !data[..data.len().min(8 << 10)].contains(&0);
    let path = name.unwrap_or_else(|| if textual { "data.txt" } else { "data" }.into());
    let member = Member {
        path,
        size: data.len() as u64,
        read: Source::Borrowed(data),
    };
    render(vec![member], Vec::new(), opts)
}

fn from_tar(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let (entries, truncated) = tar::entries(bytes, MAX_MEMBERS + 1);
    let members = entries
        .into_iter()
        .map(|e| Member {
            size: e.data.len() as u64,
            path: e.path,
            read: Source::Borrowed(e.data),
        })
        .collect();
    let mut warnings = Vec::new();
    if truncated {
        warnings.push("archive_truncated: the tar file ends early; later files are missing".into());
    }
    render(members, warnings, opts)
}

/// 系统自动生成的文件，不是内容。
fn is_noise(path: &str) -> bool {
    path.starts_with("__MACOSX/")
        || path.contains("/__MACOSX/")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|f| f == ".DS_Store" || f == "Thumbs.db")
}

fn render(
    mut members: Vec<Member<'_>>,
    mut warnings: Vec<String>,
    opts: &Options<'_>,
) -> Result<Converted, Error> {
    members.retain(|m| !is_noise(&m.path));
    members.sort_by(|a, b| a.path.cmp(&b.path));
    let total = members.len();
    let mut rows = vec![vec![
        "File".to_owned(),
        "Size".to_owned(),
        "Result".to_owned(),
    ]];
    let mut sections: Vec<Block> = Vec::new();
    let (mut converted, mut stopped) = (0usize, None);
    for m in &members {
        check_deadline(opts)?;
        let result = if let Some(why) = stopped {
            why
        } else if !take_member() {
            warnings.push(format!(
                "archive_truncated: stopped after {MAX_MEMBERS} files"
            ));
            stopped = Some("skipped: file limit");
            "skipped: file limit"
        } else if budget::exhausted() {
            stopped = Some("skipped: output limit");
            "skipped: output limit"
        } else if m.size > BYTES_LEFT.get() {
            warnings.push(format!(
                "archive_truncated: stopped after {} MB of decompressed data",
                opts.limits.max_total_bytes >> 20
            ));
            stopped = Some("skipped: size limit");
            "skipped: size limit"
        } else {
            let (result, ok) = member(m, opts, &mut sections, &mut warnings);
            converted += usize::from(ok);
            result
        };
        rows.push(vec![
            md::escape(&m.path),
            human_size(m.size),
            result.to_owned(),
        ]);
    }
    let mut blocks = Vec::with_capacity(sections.len() + 1);
    if total > 0 {
        blocks.push(Block::Table(rows));
    }
    blocks.append(&mut sections);
    Ok(Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        warnings,
        parts: converted,
        ..Default::default()
    })
}

/// 转换一个文件，内容写进 `sections`。返回清单里的说明和是否转出了内容。
fn member(
    m: &Member<'_>,
    opts: &Options<'_>,
    sections: &mut Vec<Block>,
    warnings: &mut Vec<String>,
) -> (&'static str, bool) {
    let Some(convert) = opts.member else {
        return ("listed", false);
    };
    let owned;
    let data: &[u8] = match m.read {
        Source::Borrowed(d) => d,
        Source::Zip(z) => match z.read(&m.path) {
            Ok(Some(d)) => {
                BYTES_LEFT.set(BYTES_LEFT.get().saturating_sub(d.len() as u64));
                owned = d;
                &owned
            }
            Ok(None) => return ("failed: missing", false),
            Err(Error::Encrypted) => return ("skipped: encrypted", false),
            Err(_) => return ("failed: unreadable", false),
        },
    };
    match convert(data, &m.path, budget::left()) {
        Ok(Some(doc)) => {
            warnings.extend(doc.warnings.into_iter().map(|w| format!("{}: {w}", m.path)));
            let mut body = doc.markdown;
            if body.trim().is_empty() {
                return ("empty", false);
            }
            budget::clip(&mut body);
            sections.push(Block::Heading(2, md::escape(&m.path)));
            sections.push(Block::Raw(body.trim_end().to_owned()));
            ("converted", true)
        }
        Ok(None) => ("skipped: unsupported type", false),
        Err(e) => {
            warnings.push(format!("{}: {e}", m.path));
            ("failed", false)
        }
    }
}

fn human_size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Format;

    /// 测试用的内层转换：文本照抄，压缩包递归，其他不支持。
    fn sub(bytes: &[u8], name: &str, room: usize) -> Result<Option<Converted>, Error> {
        let mut opts = Options {
            member: Some(&sub),
            ..Options::default()
        };
        opts.limits.max_output_bytes = room;
        match Format::detect(bytes, Some(name), None) {
            Some(Format::Text) => {
                crate::convert_text(std::str::from_utf8(bytes).unwrap(), Format::Text, &opts)
                    .map(Some)
            }
            Some(Format::Archive) => crate::convert(bytes, Format::Archive, &opts).map(Some),
            _ => Ok(None),
        }
    }

    fn run(bytes: &[u8]) -> Converted {
        let opts = Options {
            member: Some(&sub),
            ..Options::default()
        };
        crate::convert(bytes, Format::Archive, &opts).unwrap()
    }

    #[test]
    fn zip_lists_then_converts_in_path_order() {
        let z = zip::tests::build(&[
            ("b.txt", b"second", false),
            ("a.txt", b"first", true),
            ("dir/", b"", false),
            ("pic.png", b"\x89PNG\0", false),
            ("__MACOSX/a.txt", b"junk", false),
        ]);
        let d = run(&z);
        assert_eq!(d.parts, 2);
        assert_eq!(
            d.markdown,
            "| File | Size | Result |\n| --- | --- | --- |\n| a.txt | 5 B | converted |\n| b.txt | 6 B | converted |\n| pic.png | 5 B | skipped: unsupported type |\n\n## a.txt\n\nfirst\n\n## b.txt\n\nsecond\n"
        );
    }

    #[test]
    fn tar_and_empty_archives() {
        let mut t = tar::tests::file("n.txt", b"body", b'0');
        t.extend([0u8; 1024]);
        let d = run(&t);
        assert!(d.markdown.contains("## n.txt\n\nbody"));
        assert_eq!(run(&zip::tests::build(&[])).markdown, "");
    }

    #[test]
    fn compressed_files_and_compressed_tars() {
        let text = include_str!("../tests/data/compress/text.txt");
        for sample in [
            &include_bytes!("../tests/data/compress/text.xz")[..],
            &include_bytes!("../tests/data/compress/text.bz2")[..],
        ] {
            assert_eq!(Format::detect(sample, None, None), Some(Format::Archive));
            let d = run(sample);
            assert!(d.markdown.contains("## data.txt"), "{}", d.markdown);
            assert!(d.markdown.contains(text.lines().next().unwrap()));
        }
        let seven = include_bytes!("../tests/data/compress/files.7z");
        assert_eq!(Format::detect(seven, None, None), Some(Format::Archive));
        let d = run(seven);
        assert_eq!(d.parts, 2);
        assert!(d.markdown.contains("## dir/b.txt\n\nsecond file"));
        // 文字恰好以 BZh 开头不是压缩包
        assert_eq!(Format::detect(b"BZh is a word", None, None), None);
    }

    #[test]
    fn nesting_stops_at_the_limit() {
        let mut inner = zip::tests::build(&[("deep.txt", b"deep", false)]);
        for _ in 0..4 {
            inner = zip::tests::build(&[("in.zip", inner.as_slice(), false)]);
        }
        let d = run(&inner);
        assert!(!d.markdown.contains("deep\n"), "{}", d.markdown);
        assert!(
            d.warnings.iter().any(|w| w.contains("nesting")),
            "{:?}",
            d.warnings
        );
        assert_eq!(depth(), 0);
    }

    #[test]
    fn many_files_stop_at_the_shared_limit() {
        let names: Vec<String> = (0..MAX_MEMBERS + 5)
            .map(|i| format!("{i:05}.txt"))
            .collect();
        let parts: Vec<(&str, &[u8], bool)> = names
            .iter()
            .map(|n| (n.as_str(), b"x".as_slice(), false))
            .collect();
        let d = run(&zip::tests::build(&parts));
        assert_eq!(d.parts, MAX_MEMBERS);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.starts_with("archive_truncated"))
        );
        assert!(d.markdown.contains("skipped: file limit"));
    }

    #[test]
    fn output_budget_is_shared_with_members() {
        let big = vec![b'y'; 4096];
        let z = zip::tests::build(&[
            ("a.txt", big.as_slice(), false),
            ("b.txt", big.as_slice(), false),
        ]);
        let mut opts = Options {
            member: Some(&sub),
            ..Options::default()
        };
        opts.limits.max_output_bytes = 5000;
        let d = crate::convert(&z, Format::Archive, &opts).unwrap();
        assert!(d.markdown.len() <= 5000);
        assert!(d.warnings.iter().any(|w| w.starts_with("output_truncated")));
    }

    #[test]
    fn without_a_member_converter_it_only_lists() {
        let z = zip::tests::build(&[("a.txt", b"x", false)]);
        let d = crate::convert(&z, Format::Archive, &Options::default()).unwrap();
        assert!(d.markdown.contains("| a.txt | 1 B | listed |"));
        assert!(!d.markdown.contains("## a.txt"));
    }
}
