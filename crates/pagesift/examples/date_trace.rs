//! 调试发布日期：列出每一步各自找到的日期。`date_trace <html 文件> [URL]`
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: date_trace <file> [url]");
    let url = args.next();
    let html = pagesift_net::decode(&std::fs::read(path).unwrap(), None, None).text;
    for (stage, date, _) in pagesift_content::trace_published(&html, url.as_deref()) {
        println!("{stage:10} {}", date.unwrap_or_default());
    }
}
