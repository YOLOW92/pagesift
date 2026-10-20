//! 把目录里每个页面的完整处理结果（所有开关打开）写成文本，用来比较两个版本的输出。
//! `cargo run --release -p pagesift-content --example snapshot -- bench/data/inputs out/`

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: snapshot <in-dir> <out-dir>");
    let out = std::path::PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    for main_only in [true, false] {
        let opts = pagesift_content::Options {
            base_url: Some("https://example.com/a/b".parse().unwrap()),
            only_main_content: main_only,
            plain_text: true,
            clean_html: true,
            links: true,
            images: true,
            fingerprints: true,
            ..Default::default()
        };
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let html = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            let doc = pagesift_content::process(&html, &opts);
            let name = format!(
                "{}.{}.txt",
                path.file_stem().unwrap().to_string_lossy(),
                if main_only { "main" } else { "full" }
            );
            std::fs::write(out.join(name), format!("{doc:#?}")).unwrap();
        }
    }
}
