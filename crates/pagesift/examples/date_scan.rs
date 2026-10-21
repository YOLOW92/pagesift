//! 崩溃扫描：对目录里的每个页面跑发布日期的全部步骤（不在第一个结果处停），报告崩溃的文件和总耗时。
//! `date_scan <目录>...`
fn main() {
    let mut files = Vec::new();
    for dir in std::env::args().skip(1) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            files.push(e.path());
        }
    }
    let t = std::time::Instant::now();
    let mut crashes = 0;
    let mut per_stage: Vec<(&'static str, u64)> = Vec::new();
    for f in &files {
        let Ok(bytes) = std::fs::read(f) else {
            continue;
        };
        let html = pagesift_net::decode(&bytes, None, None).text;
        match std::panic::catch_unwind(|| pagesift_content::trace_published(&html, None)) {
            Ok(stages) => {
                for (i, (name, _, ns)) in stages.into_iter().enumerate() {
                    if per_stage.len() <= i {
                        per_stage.push((name, 0));
                    }
                    per_stage[i].1 += ns;
                }
            }
            Err(_) => {
                crashes += 1;
                eprintln!("CRASH {}", f.display());
            }
        }
    }
    let mut early = 0u64;
    let mut whole = 0u64;
    for f in &files {
        let Ok(bytes) = std::fs::read(f) else {
            continue;
        };
        let html = pagesift_net::decode(&bytes, None, None).text;
        early += pagesift_content::time_published(&html, None).1;
        let t = std::time::Instant::now();
        let _ = pagesift_content::process(&html, &Default::default());
        whole += t.elapsed().as_nanos() as u64;
    }
    println!(
        "published (stop at first): {:.3} ms/page; whole process(): {:.3} ms/page",
        early as f64 / 1e6 / files.len() as f64,
        whole as f64 / 1e6 / files.len() as f64
    );
    for (name, ns) in &per_stage {
        println!(
            "{name:10} {:8.3} ms/page",
            *ns as f64 / 1e6 / files.len() as f64
        );
    }
    println!(
        "{} pages, {crashes} crashes, {:.2} ms/page (all stages)",
        files.len(),
        t.elapsed().as_secs_f64() * 1000.0 / files.len() as f64
    );
}
