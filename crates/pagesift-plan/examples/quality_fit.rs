//! 拟合质量模型并做 DESIGN §9.3 的校准判定。
//!
//! `cargo run --release -p pagesift-plan --example quality_fit -- <features.jsonl> <recall.jsonl> <model.bin> <report.json>`
//!
//! - 标签（§6.4）：召回率不低于同一数据集召回率分布的 `q(δ)` 为正例。
//! - 三折交叉验证：按 id 的哈希把页面分成三份，每份由另外两份交叉拟合的模型（[`QualityModel::fit_cross`]）
//!   预测；三份的样本外预测合在一起判定（每页都只被没见过它的模型预测，判定用上全部页面，
//!   不依赖哪一份碰巧负例多）。逐折结果也写进报告。
//! - 判定：可靠性（每箱观测频率与平均预测在 BH 控制下无显著差）与区分度
//!   （Brier 显著低于常数基准率预测器，配对 bootstrap）同时满足才算达标。
//! - 写出的模型用全部页面拟合（按 id 哈希的奇偶分成两半做交叉拟合）。
use std::collections::HashMap;

use pagesift_plan::quality::{FEATURE_NAMES, Features, QualityModel};
use pagesift_stat::Delta;
use pagesift_stat::rng::Rng;
use pagesift_stat::verdict::{brier, calibrated};

fn fnv(s: &str) -> u64 {
    // FNV-1a（Fowler–Noll–Vo）：只用来稳定地分组，不需要抗碰撞
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [feat_path, recall_path, model_out, report_out] = args.as_slice() else {
        panic!("usage: quality_fit <features.jsonl> <recall.jsonl> <model.bin> <report.json>");
    };
    let delta = Delta::DEFAULT;
    let read = |p: &str| -> Vec<serde_json::Value> {
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let recall: HashMap<String, f64> = read(recall_path)
        .into_iter()
        .filter_map(|r| Some((r["id"].as_str()?.to_string(), r["recall"].as_f64()?)))
        .collect();
    // 每个数据集的 q(δ) 门槛
    let mut by_set: HashMap<String, Vec<f64>> = HashMap::new();
    let rows: Vec<(String, String, Features)> = read(feat_path)
        .into_iter()
        .filter_map(|r| {
            let id = r["id"].as_str()?.to_string();
            recall.get(&id)?;
            let f: Vec<f64> = r["features"]
                .as_array()?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0))
                .collect();
            Some((id, r["set"].as_str()?.to_string(), f.try_into().ok()?))
        })
        .collect();
    for (id, set, _) in &rows {
        by_set.entry(set.clone()).or_default().push(recall[id]);
    }
    let cut: HashMap<String, f64> = by_set
        .iter_mut()
        .map(|(s, v)| {
            v.sort_by(f64::total_cmp);
            (
                s.clone(),
                v[((delta.get() * (v.len() - 1) as f64).floor()) as usize],
            )
        })
        .collect();
    let mut parts: [Vec<(Features, bool)>; 3] = Default::default();
    let mut halves: [Vec<(Features, bool)>; 2] = Default::default();
    for (id, set, f) in &rows {
        // "不低于"门槛即为正例（DESIGN §6.4）。召回为 0（标准答案一句都没取到）无论门槛在哪都不算完整：
        // 这是定义上的下限，Firecrawl 集 22% 的页召回为 0、q(δ) 落在 0 上时起作用
        let y = recall[id] >= cut[set] && recall[id] > 0.0;
        parts[(fnv(id) % 3) as usize].push((*f, y));
        halves[(fnv(id) % 2) as usize].push((*f, y));
    }
    let mut rng = Rng::seed(fnv("quality_fit"));
    let (mut probs, mut ys) = (Vec::new(), Vec::new());
    let mut folds = Vec::new();
    for h in 0..3 {
        let (a, b) = (&parts[(h + 1) % 3], &parts[(h + 2) % 3]);
        let m = QualityModel::fit_cross(a, b).expect("not enough data to fit");
        let (p, y): (Vec<f64>, Vec<bool>) =
            parts[h].iter().map(|(f, y)| (m.predict(f), *y)).unzip();
        let (ok, _, d) = calibrated(&p, &y, delta, &mut rng);
        folds.push(serde_json::json!({"fold": h, "pages": p.len(), "pass": ok, "brier_diff_interval": [d.lo, d.estimate, d.hi]}));
        probs.extend(p);
        ys.extend(y);
    }
    let (ok, bins, diff) = calibrated(&probs, &ys, delta, &mut rng);
    let base = ys.iter().filter(|&&y| y).count() as f64 / ys.len() as f64;
    let model = QualityModel::fit_cross(&halves[0], &halves[1]).expect("not enough data to fit");
    let report = serde_json::json!({
        "delta": delta.get(),
        "pages": rows.len(),
        "split": parts.iter().map(Vec::len).collect::<Vec<_>>(),
        "recall_cut_by_set": cut,
        "positive_rate": base,
        "brier_model": brier(&probs, &ys),
        "brier_constant": brier(&vec![base; ys.len()], &ys),
        "brier_diff_interval": [diff.lo, diff.estimate, diff.hi],
        "bins": bins.iter().map(|b| serde_json::json!({"mean_pred": b.mean_pred, "observed": b.observed, "n": b.n, "wilson": [b.lo, b.hi]})).collect::<Vec<_>>(),
        "folds": folds,
        "weights": model.scores.iter().map(|m| FEATURE_NAMES.iter().zip(&m.w).map(|(n, w)| (n.to_string(), serde_json::json!(w))).collect::<serde_json::Map<_, _>>()).collect::<Vec<_>>(),
        "pass": ok,
    });
    std::fs::write(model_out, model.encode()).unwrap();
    std::fs::write(report_out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!(
        "quality_fit: {} pages, 3-fold out-of-fold (positive {:.3}), pass = {ok}",
        rows.len(),
        base
    );
}
