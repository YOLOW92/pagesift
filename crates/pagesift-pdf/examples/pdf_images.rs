//! 图片识别与导出、按页质量分的检查工具：
//!
//!     cargo run --profile release-fast -p pagesift-pdf --example pdf_images -- <out 目录> <pdf>...
//!
//! 每份 PDF 打开 list + markdown + export，把导出的图片写到 `<out>/images/<文件名>`，
//! Markdown 写到 `<out>/<pdf 名>.md`，每行打印一张图（TSV）和每个复核过的页。

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = Path::new(&args[0]);
    std::fs::create_dir_all(out.join("images")).unwrap();
    let opts = pagesift_pdf::Options {
        images: pagesift_pdf::ImageOptions {
            list: true,
            markdown: true,
            export: true,
        },
        ..Default::default()
    };
    for path in &args[1..] {
        let bytes = std::fs::read(path).unwrap();
        let name = Path::new(path).file_stem().unwrap().to_string_lossy();
        match pagesift_pdf::extract_with(&bytes, &opts) {
            Ok(doc) => {
                std::fs::write(out.join(format!("{name}.md")), &doc.markdown).unwrap();
                for img in &doc.images {
                    if let Some(d) = &img.data {
                        std::fs::write(out.join("images").join(img.file_name()), d).unwrap();
                    }
                    println!(
                        "image\t{name}\t{}\t{}\t{}x{}\t{}\t{}\t{}\t{}\t{}",
                        img.file_name(),
                        img.encoding.name(),
                        img.width,
                        img.height,
                        img.bits_per_component,
                        img.color_space,
                        img.bytes,
                        img.placements.len(),
                        img.export_error.as_deref().unwrap_or("-")
                    );
                }
                for p in &doc.pages {
                    if p.quality.rechecked || p.scanned {
                        println!(
                            "page\t{name}\t{}\tscanned={}\trechecked={}\treplaced={}\t{:?}",
                            p.number, p.scanned, p.quality.rechecked, p.quality.replaced, p.quality
                        );
                    }
                }
            }
            Err(e) => println!("error\t{name}\t{e}"),
        }
    }
}
