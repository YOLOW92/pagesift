//! 用法：cargo run -p pagesift-pdf --example pdf2md -- file.pdf

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: pdf2md <file.pdf>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let start = std::time::Instant::now();
    match pagesift_pdf::extract(&bytes) {
        Ok(doc) => {
            print!("{}", doc.markdown);
            eprintln!(
                "pages={} scanned={} title={:?} author={:?} created={:?} elapsed={:?}",
                doc.pages.len(),
                doc.scanned,
                doc.metadata.title,
                doc.metadata.author,
                doc.metadata.creation_date,
                start.elapsed()
            );
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    }
}
