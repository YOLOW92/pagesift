//! 把 PDF 转成 Markdown 写到目录里（标题评测用）：`pdf_md <输出目录> [--inferred] <file.pdf>...`
//! 输出文件名是 `<名字>.md`。`--inferred` 不读结构树和书签，只看版面推断。
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(args.remove(0));
    let inferred = args.first().is_some_and(|a| a == "--inferred");
    if inferred {
        args.remove(0);
    }
    std::fs::create_dir_all(&out).expect("mkdir");
    let opts = pagesift_pdf::Options::default();
    for path in args {
        let bytes = std::fs::read(&path).expect("read");
        let r = if inferred {
            pagesift_pdf::fuzzing::extract_inferred(&bytes, &opts)
        } else {
            pagesift_pdf::extract_with(&bytes, &opts)
        };
        let md = r.map(|d| d.markdown).unwrap_or_default();
        let stem = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::fs::write(out.join(format!("{stem}.md")), md).expect("write");
    }
}
