//! 退回 CPU：`PAGESIFT_GPU=off`、驱动载入失败、显存几乎占满，都要能出结果并如实报告。
//! 改环境变量影响整个进程，所以这个测试程序只有一个测试函数，各步骤顺序执行。

use std::path::PathBuf;

use pagesift_ocr::pipeline::Engine;
use pagesift_ocr::{Device, Image, Line};

fn dirs() -> Option<(PathBuf, PathBuf)> {
    let root = pagesift_ocr::models::cache_root()?;
    let d = root.join("PP-OCRv6_small_det_safetensors");
    let r = root.join("PP-OCRv6_small_rec_safetensors");
    (d.join("model.safetensors").exists() && r.join("model.safetensors").exists()).then_some((d, r))
}

/// 白底上几行黑块，够让检测框出行。
fn page() -> Image {
    let (width, height) = (800, 600);
    let mut data = vec![240u8; width * height];
    for row in (60..540).step_by(60) {
        for x in (60..740).step_by(14) {
            for y in row..row + 24 {
                for p in &mut data[y * width + x..y * width + x + 9] {
                    *p = 20;
                }
            }
        }
    }
    Image {
        width,
        height,
        channels: 1,
        data,
    }
}

fn texts(l: &[Line]) -> Vec<&str> {
    l.iter().map(|l| l.text.as_str()).collect()
}

/// 没有显卡的环境也要跑前两步，它们不需要显卡。
#[test]
fn 退回_cpu_也能出结果并报告原因() {
    let Some((det, rec)) = dirs() else {
        eprintln!("跳过：缓存里没有 small 权重");
        return;
    };
    let img = page();
    let gpu = Device::Gpu { index: 0 };
    let cpu_lines = Engine::load(&det, &rec).unwrap().run(&img).unwrap();

    // SAFETY: 这个测试程序只有这一个测试函数，改环境变量时没有别的线程在读
    unsafe { std::env::set_var("PAGESIFT_GPU", "off") };
    let e = Engine::load_with(&det, &rec, gpu).unwrap();
    let s = e.device_status();
    assert_eq!(s.actual, Device::Cpu);
    assert!(s.fallback.as_deref().unwrap().contains("PAGESIFT_GPU=off"));
    assert_eq!(texts(&e.run(&img).unwrap()), texts(&cpu_lines));
    assert!(e.gpu_stats().is_none());
    drop(e);
    unsafe { std::env::remove_var("PAGESIFT_GPU") };

    // 模拟驱动载入失败
    unsafe { std::env::set_var("PAGESIFT_GPU_LIB", "/nonexistent/libcuda.so.1") };
    let e = Engine::load_with(&det, &rec, gpu).unwrap();
    let s = e.device_status();
    assert_eq!(s.actual, Device::Cpu);
    assert!(s.fallback.as_deref().unwrap().contains("failed to load"));
    assert_eq!(texts(&e.run(&img).unwrap()), texts(&cpu_lines));
    drop(e);
    unsafe { std::env::remove_var("PAGESIFT_GPU_LIB") };

    // 不存在的序号
    let e = Engine::load_with(&det, &rec, Device::Gpu { index: 9999 }).unwrap();
    assert_eq!(e.device_status().actual, Device::Cpu);
    assert!(e.device_status().fell_back());
    drop(e);

    // 以下需要显卡
    if pagesift_gpu::device_count() == 0 {
        eprintln!("没有 N 卡，跳过 GPU 和显存占满两步");
        return;
    }
    let e = Engine::load_with(&det, &rec, gpu).unwrap();
    let s = e.device_status();
    assert_eq!(s.actual, gpu, "有卡却退回了：{:?}", s.fallback);
    assert!(!s.fell_back());
    assert_eq!(texts(&e.run(&img).unwrap()), texts(&cpu_lines));
    let stats = e.gpu_stats().unwrap();
    assert!(stats.batches > 0, "{stats:?}");
    assert!(stats.lines > 0, "{stats:?}");
    drop(e);

    // 显存几乎占满：同一进程共用主上下文，另开一个 Gpu 把显存吃到只剩一点，再新建引擎
    // （已有的引擎缓冲池里留着显存，同样大小的输入用不着再分配）
    let hog = pagesift_gpu::Gpu::open().unwrap();
    let mut held = Vec::new();
    // [策略] 按 256 MiB 一块吃，失败就减半，把剩余吃到 64 KiB 以内
    let mut chunk = 64 << 20;
    while chunk >= 1 << 14 {
        match hog.alloc(chunk) {
            Ok(b) => held.push(b),
            Err(_) => chunk /= 2,
        }
    }
    let e = Engine::load_with(&det, &rec, gpu).unwrap();
    let r = e.run_until(&img, None).unwrap();
    assert_eq!(texts(&r.lines), texts(&cpu_lines));
    eprintln!(
        "显存占满后：{:?}，退回原因：{:?}",
        e.gpu_stats(),
        r.fallback
    );
    assert!(r.fallback.is_some(), "显存占满却没有报告退回");
    drop(held);
}
