//! 内置质量模型对绝对标签的校准检查（0.91）：质量分是不是"结果完整"的概率。
//!
//! `cargo run --release -p pagesift-plan --example quality_eval -- <features.jsonl> <recall.jsonl>`
//!
//! `features.jsonl` 是 `quality_data` 的输出，`recall.jsonl` 每行 `{id, recall}`。对 τ = 0.5、0.8、0.9，
//! 标签"召回 ≥ τ"，按预测分四分位分箱，输出每箱平均预测、观测比例，以及模型和常数（基准率）预测的 Brier
//! 分数（越低越好）。输出一个 JSON。
use std::collections::HashMap;

use pagesift_plan::quality::{Features, QualityModel};
use pagesift_stat::verdict::brier;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [feat_path, recall_path] = args.as_slice() else {
        panic!("usage: quality_eval <features.jsonl> <recall.jsonl>");
    };
    let model = QualityModel::builtin().expect("built-in quality model");
    let recall: HashMap<String, f64> = std::fs::read_to_string(recall_path)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            Some((v["id"].as_str()?.to_string(), v["recall"].as_f64()?))
        })
        .collect();
    let mut rows: Vec<(f64, f64)> = std::fs::read_to_string(feat_path)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let r = *recall.get(v["id"].as_str()?)?;
            let f: Features = serde_json::from_value(v["features"].clone()).ok()?;
            Some((model.predict(&f), r))
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let p: Vec<f64> = rows.iter().map(|r| r.0).collect();
    let mut out = serde_json::json!({"pages": rows.len()});
    // [推导] 0.83 校准报告同样分四箱（每箱约 n/4 页）
    let bins = 4;
    for tau in [0.5, 0.8, 0.9] {
        let y: Vec<bool> = rows.iter().map(|r| r.1 >= tau).collect();
        let base = y.iter().filter(|&&b| b).count() as f64 / y.len() as f64;
        let table: Vec<_> = (0..bins)
            .map(|b| {
                let (lo, hi) = (b * rows.len() / bins, (b + 1) * rows.len() / bins);
                let n = (hi - lo) as f64;
                serde_json::json!({
                    "pages": hi - lo,
                    "mean_prediction": p[lo..hi].iter().sum::<f64>() / n,
                    "observed": y[lo..hi].iter().filter(|&&b| b).count() as f64 / n,
                })
            })
            .collect();
        out[format!("recall>={tau}")] = serde_json::json!({
            "base_rate": base,
            "brier_model": brier(&p, &y),
            "brier_constant": brier(&vec![base; y.len()], &y),
            "bins": table,
        });
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
