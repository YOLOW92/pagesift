//! 对照 libjpeg-turbo（经 Pillow 解码）的结果。testdata/ 里的 .jpg 和 .raw 由 Pillow 生成：
//! 37×29（宽高都不是 8 的倍数），渐变加实心块和斜线。

use crate::image::decode::Raster;

/// [策略] 测试图都很小，给 PDF 默认的流大小上限
const MAX: usize = 64 << 20;

fn decode(data: &[u8], ct: Option<bool>) -> Result<Raster, String> {
    super::decode(data, ct, MAX)
}

/// [协议] IEEE 1180-1990：IDCT 每个像素和理想值的峰值误差不超过 1；两边各 1，相差至多 2。
/// 再加 YCbCr→RGB 两边各自取整的 1（libjpeg 用定点、我们用浮点）。
const TOL_GRAY: u8 = 2;
const TOL_COLOR: u8 = 3;

fn raw(name: &str) -> (Raster, Vec<u8>) {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/image/decode/jpeg/testdata/"
    );
    let jpg = std::fs::read(format!("{dir}{name}.jpg")).unwrap();
    let want = std::fs::read(format!("{dir}{name}.raw")).unwrap();
    (decode(&jpg, None).unwrap(), want)
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len(), "长度不同");
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
}

fn check(name: &str, channels: u8, tol: u8) {
    let (got, want) = raw(name);
    assert_eq!(
        (got.width, got.height, got.channels),
        (37, 29, channels),
        "{name}"
    );
    let d = max_diff(&got.data, &want);
    assert!(d <= tol, "{name}：最大差 {d} > {tol}");
}

/// 有色度子采样时只比 Y 通道：libjpeg 默认用三角插值放大色度，我们用最近邻，色度和换出的 RGB
/// 都会不同（饱和处截断的程度也不同），Y 不受影响。参照 .y 是 Pillow 以 YCbCr 模式解出的 Y。
fn check_luma(name: &str) {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/image/decode/jpeg/testdata/"
    );
    let jpg = std::fs::read(format!("{dir}{name}.jpg")).unwrap();
    let want = std::fs::read(format!("{dir}{name}.y")).unwrap();
    let got = decode(&jpg, Some(false)).unwrap();
    let y: Vec<u8> = got.data.as_chunks::<3>().0.iter().map(|p| p[0]).collect();
    let d = max_diff(&y, &want);
    assert!(d <= TOL_GRAY, "{name}：Y 最大差 {d} > {TOL_GRAY}");
}

#[test]
fn baseline_gray() {
    check("gray", 1, TOL_GRAY);
}

#[test]
fn baseline_444() {
    check("ycc444", 3, TOL_COLOR);
}

#[test]
fn baseline_subsampled() {
    check_luma("ycc422");
    check_luma("ycc420");
}

#[test]
fn restart_interval() {
    check_luma("restart");
}

#[test]
fn progressive() {
    check("proggray", 1, TOL_GRAY);
    check("prog444", 3, TOL_COLOR);
}

#[test]
fn adobe_cmyk() {
    check("cmyk", 4, TOL_GRAY);
}

#[test]
fn rejects_garbage() {
    assert!(decode(b"not a jpeg", None).is_err());
    assert!(decode(&[0xFF, 0xD8, 0xFF, 0xD9], None).is_err());
    // 截断：只留前一半
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/image/decode/jpeg/testdata/"
    );
    let jpg = std::fs::read(format!("{dir}gray.jpg")).unwrap();
    let _ = decode(&jpg[..jpg.len() / 2], None); // 不能崩溃；结果对错不要求
}

#[test]
fn rejects_oversized_frame() {
    // gray.jpg 是 37×29：系数 5×4 块 ×64×2 字节 + 1073 字节像素，上限比它小一个字节就该拒绝
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/image/decode/jpeg/testdata/"
    );
    let jpg = std::fs::read(format!("{dir}gray.jpg")).unwrap();
    let need = 5 * 4 * 64 * 2 + 37 * 29;
    assert!(super::decode(&jpg, None, need).is_ok());
    assert!(super::decode(&jpg, None, need - 1).is_err());
}
