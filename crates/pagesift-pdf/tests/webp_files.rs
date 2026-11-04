//! 和 Pillow（libwebp）解出的灰度逐像素对照。样本由外部脚本生成，目录放在 `PAGESIFT_WEBP_CASES`，没设就跳过。

use std::path::Path;

#[test]
fn webp_matches_reference() {
    let Ok(dir) = std::env::var("PAGESIFT_WEBP_CASES") else {
        return;
    };
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.file_name().to_str()?.to_owned()))
        .filter(|n| n.ends_with(".webp"))
        .collect();
    names.sort();
    let mut bad = 0;
    for name in &names {
        let base = Path::new(&dir);
        let bytes = std::fs::read(base.join(name)).unwrap();
        let want = std::fs::read(base.join(format!("{name}.gray"))).unwrap();
        let got = match pagesift_pdf::decode_image_file(&bytes, 100_000_000) {
            Ok(g) => g,
            Err(e) => {
                println!("{name}: 错误 {e}");
                bad += 1;
                continue;
            }
        };
        assert_eq!(got.pixels.len(), want.len(), "{name}");
        let sum: u64 = got
            .pixels
            .iter()
            .zip(&want)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        let max = got
            .pixels
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let mae = sum as f64 / want.len().max(1) as f64;
        println!("{name}: 平均差 {mae:.3}，最大差 {max}");
        // 无损只允许亮度取整差 1；有损不做环路滤波，平均差要小
        let limit = if name.contains("_ll") || name.contains("rgba") || name.contains("pal") {
            1.0
        } else {
            6.0
        };
        if mae > limit {
            bad += 1;
        }
    }
    assert_eq!(bad, 0);
}
