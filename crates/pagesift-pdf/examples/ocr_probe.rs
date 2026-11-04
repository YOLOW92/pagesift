//! 检查扫描页能不能解成灰度图交给 OCR：用一个只记录输入的假引擎跑目录里的每个 PDF，
//! 打印每页交给引擎的图的宽、高和深色像素占比。
//!     ocr_probe <pdf 目录>
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Probe(Mutex<Vec<(u32, u32, f64)>>);

impl pagesift_pdf::PageOcr for Probe {
    fn recognize(
        &self,
        w: u32,
        h: u32,
        gray: &[u8],
        _deadline: Option<std::time::Instant>,
    ) -> Result<pagesift_pdf::OcrOutput, String> {
        let dark = gray.iter().filter(|&&v| v < 128).count() as f64 / gray.len().max(1) as f64;
        self.0.lock().unwrap().push((w, h, dark));
        Ok(Default::default())
    }
}

fn main() {
    let dir = std::env::args().nth(1).expect("pdf 目录");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let probe = Arc::new(Probe::default());
    let opts = pagesift_pdf::Options {
        ocr: Some(probe.clone()),
        ..Default::default()
    };
    for f in files
        .iter()
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
    {
        probe.0.lock().unwrap().clear();
        let r = pagesift_pdf::extract_with(&std::fs::read(f).unwrap(), &opts);
        let seen = probe.0.lock().unwrap().clone();
        let name = f.file_name().unwrap().to_string_lossy();
        match r {
            Ok(_) if seen.is_empty() => println!("{name}\t没交给引擎"),
            Ok(_) => println!(
                "{name}\t{}",
                seen.iter()
                    .map(|(w, h, d)| format!("{w}x{h} 深色{:.0}%", d * 100.0))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Err(e) => println!("{name}\t错误 {e}"),
        }
    }
}
