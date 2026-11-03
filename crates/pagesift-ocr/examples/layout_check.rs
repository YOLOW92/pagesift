//! 版面检测检查：载入模型，读一张 PGM/PPM（P5/P6）图，把区域写成文本（每行 x0 y0 x1 y1 分数 类别 细分类）。
//!     cargo run --profile release-fast -p pagesift-ocr --example layout_check -- <模型目录> <图> <输出> [分数阈值]
//! 耗时打到标准错误。
use std::fmt::Write;
use std::time::Instant;

/// 读 P5/P6：跳过注释，头之后是一个空白再接像素。
fn read_pnm(path: &str) -> pagesift_ocr::Image {
    let b = std::fs::read(path).unwrap();
    let mut i = 0;
    let mut token = || {
        loop {
            while b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b[i] != b'#' {
                break;
            }
            while b[i] != b'\n' {
                i += 1;
            }
        }
        let s = i;
        while !b[i].is_ascii_whitespace() {
            i += 1;
        }
        String::from_utf8_lossy(&b[s..i]).to_string()
    };
    let magic = token();
    let channels = match magic.as_str() {
        "P5" => 1,
        "P6" => 3,
        m => panic!("不支持的格式 {m}"),
    };
    let width: usize = token().parse().unwrap();
    let height: usize = token().parse().unwrap();
    let max: usize = token().parse().unwrap();
    assert_eq!(max, 255, "只支持 8 位图");
    let data = b[i + 1..i + 1 + width * height * channels].to_vec();
    pagesift_ocr::Image {
        width,
        height,
        channels,
        data,
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    assert!(
        a.len() == 3 || a.len() == 4,
        "用法：layout_check <模型目录> <图> <输出> [分数阈值]"
    );
    let t = Instant::now();
    let mut m = pagesift_ocr::layout::Layout::load(std::path::Path::new(&a[0])).unwrap();
    if let Some(th) = a.get(3) {
        m.threshold = th.parse().unwrap();
    }
    eprintln!("载入 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let img = read_pnm(&a[1]);
    // 设了 LAYOUT_RAW 就打印原始输出的左上角，和参考实现的集成测试对照
    if std::env::var_os("LAYOUT_RAW").is_some() {
        let r = m.raw(&img).unwrap();
        let (_, k) = r.logits.tc();
        let (q, _) = r.order.tc();
        for i in 0..3 {
            println!("logits {:?}", &r.logits.data[i * k..i * k + 3]);
            println!("boxes {:?}", &r.boxes[i][..3]);
            println!("order {:?}", &r.order.data[i * q..i * q + 3]);
        }
    }
    let t = Instant::now();
    let rs = m.detect(&img).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "{}: {}×{}，{} 个区域，{ms:.0} ms",
        a[1],
        img.width,
        img.height,
        rs.len()
    );
    let mut s = String::new();
    for r in &rs {
        writeln!(
            s,
            "{:.4} {:.4} {:.4} {:.4} {:.4} {} {}",
            r.x0, r.y0, r.x1, r.y1, r.score, r.label, r.kind
        )
        .unwrap();
    }
    std::fs::write(&a[2], s).unwrap();
}
