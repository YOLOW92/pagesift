use pagesift_diff::*;

fn kinds(r: &DiffReport) -> Vec<ChangeType> {
    r.blocks.iter().map(|b| b.change).collect()
}

#[test]
fn identical() {
    let a = "# T\n\nhello\n\n- a\n- b\n";
    let r = diff(a, a);
    assert!(!r.changed && !r.substantive && r.blocks.is_empty());
    assert_eq!(r.stats.unchanged, 4);
}

#[test]
fn whitespace_only_is_unchanged() {
    let r = diff("hello   world\n\nfoo", "hello world\n\n\n\nfoo\n");
    assert!(!r.changed);
}

#[test]
fn pure_addition() {
    let r = diff("one\n\ntwo", "one\n\nnew para\n\ntwo");
    assert_eq!(kinds(&r), [ChangeType::Added]);
    assert!(r.substantive);
    assert_eq!(r.blocks[0].new_index, Some(1));
}

#[test]
fn pure_removal() {
    let r = diff("one\n\nold para\n\ntwo", "one\n\ntwo");
    assert_eq!(kinds(&r), [ChangeType::Removed]);
    assert_eq!(r.blocks[0].old_text.as_deref(), Some("old para"));
}

#[test]
fn moved_paragraph() {
    let a = "alpha paragraph here\n\nbeta paragraph there\n\ngamma paragraph again\n\ndelta ending";
    let b = "beta paragraph there\n\ngamma paragraph again\n\ndelta ending\n\nalpha paragraph here";
    let r = diff(a, b);
    assert_eq!(kinds(&r), [ChangeType::Moved]);
    assert_eq!(r.blocks[0].old_index, Some(0));
    assert_eq!(r.blocks[0].new_index, Some(3));
    let r = diff_with(
        a,
        b,
        &DiffOptions {
            moved_is_noise: true,
            ..Default::default()
        },
    );
    assert!(!r.substantive && r.changed);
}

#[test]
fn price_only_is_noise() {
    let r = diff(
        "# Shop\n\nWidget now $1,299.00 while stocks last",
        "# Shop\n\nWidget now $1,199.50 while stocks last",
    );
    assert_eq!(kinds(&r), [ChangeType::Modified]);
    assert_eq!(r.blocks[0].significance, Significance::Noise);
    assert_eq!(r.blocks[0].noise_categories, [NoiseCategory::Price]);
    assert!(r.changed && !r.substantive);
}

#[test]
fn price_configurable_as_substantive() {
    let opts = DiffOptions {
        noise_categories: vec![
            NoiseCategory::Date,
            NoiseCategory::RelativeTime,
            NoiseCategory::Count,
        ],
        ..Default::default()
    };
    let r = diff_with("Widget now $10 only", "Widget now $12 only", &opts);
    assert!(r.substantive);
    assert_eq!(r.blocks[0].significance, Significance::Substantive);
}

#[test]
fn relative_time_only_is_noise() {
    let r = diff("Posted 3 hours ago by admin", "Posted 5 hours ago by admin");
    assert_eq!(r.blocks[0].noise_categories, [NoiseCategory::RelativeTime]);
    assert!(!r.substantive);
    let r = diff("发布于 3 小时前", "发布于 4 小时前");
    assert_eq!(r.blocks[0].noise_categories, [NoiseCategory::RelativeTime]);
}

#[test]
fn count_and_date_noise() {
    let r = diff("1,234 次阅读 · 12 条评论", "1,301 次阅读 · 15 条评论");
    assert_eq!(r.blocks[0].noise_categories, [NoiseCategory::Count]);
    let r = diff("更新于 2024年1月5日", "更新于 2024年3月9日");
    assert_eq!(r.blocks[0].noise_categories, [NoiseCategory::Date]);
}

#[test]
fn chinese_single_char_edit() {
    let r = diff(
        "这家餐厅的服务态度非常好，推荐大家来试试。",
        "这家餐厅的服务态度非常差，推荐大家来试试。",
    );
    assert_eq!(kinds(&r), [ChangeType::Modified]);
    let b = &r.blocks[0];
    assert_eq!(b.significance, Significance::Substantive);
    let del: Vec<_> = b
        .inline
        .iter()
        .filter(|s| s.op == InlineOp::Delete)
        .collect();
    let ins: Vec<_> = b
        .inline
        .iter()
        .filter(|s| s.op == InlineOp::Insert)
        .collect();
    assert_eq!((del[0].text.as_str(), ins[0].text.as_str()), ("好", "差"));
}

#[test]
fn table_row_edit() {
    let a = "| name | stock |\n|---|---|\n| apple | in stock |\n| pear | in stock |\n| plum | in stock |";
    let b = "| name | stock |\n|---|---|\n| apple | in stock |\n| pear | sold out |\n| plum | in stock |";
    let r = diff(a, b);
    assert_eq!(kinds(&r), [ChangeType::Modified]);
    assert_eq!(r.blocks[0].block_type, BlockKind::TableRow);
    assert_eq!(r.blocks[0].significance, Significance::Substantive);
}

#[test]
fn completely_different() {
    let r = diff("aaa one\n\nbbb two\n\nccc three", "xxx uno\n\nyyy dos");
    assert_eq!(r.stats.added, 2);
    assert_eq!(r.stats.removed, 3);
    assert_eq!(r.stats.modified, 0);
    assert!(r.substantive);
}

#[test]
fn serializes_camel_case() {
    let r = diff("Price $5 today", "Price $6 today");
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["changed"], true);
    assert_eq!(v["substantive"], false);
    assert_eq!(v["blocks"][0]["type"], "modified");
    assert_eq!(v["blocks"][0]["blockType"], "paragraph");
    assert_eq!(v["blocks"][0]["significance"], "noise");
    assert_eq!(v["blocks"][0]["noiseCategories"][0], "price");
    assert_eq!(v["stats"]["oldBlocks"], 1);
}

#[test]
fn options_deserialize_partial() {
    let o: DiffOptions = serde_json::from_str(r#"{"noiseCategories":["date"]}"#).unwrap();
    assert_eq!(o.noise_categories, [NoiseCategory::Date]);
    assert_eq!(o.similarity_threshold, 0.5);
}
