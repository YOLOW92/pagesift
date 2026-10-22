//! Evaluate saved business anchors against a manifest without modifying it.
use pagesift_extract::{Anchor, Relocation};
use pagesift_html::Html;
use std::io::Write;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: relocate_jsonl original.html selector manifest.jsonl output.jsonl".into(),
        );
    }
    let original = std::fs::read_to_string(&args[1])?;
    let old = Html::parse(&original);
    let anchor = Anchor::capture(&old, &args[2], "data-item-id")?;
    let mut out = std::fs::File::create(&args[4])?;
    for line in std::fs::read_to_string(&args[3])?.lines() {
        let row: serde_json::Value = serde_json::from_str(line)?;
        let source = std::fs::read_to_string(row["path"].as_str().ok_or("missing path")?)?;
        let doc = Html::parse(&source);
        let (status, values) = match anchor.locate(&doc) {
            Relocation::Exact(n) => ("exact", vec![n.text()]),
            Relocation::Relocated(n) => ("relocated", vec![n.text()]),
            Relocation::Missing => ("missing", vec![]),
            Relocation::Ambiguous => ("ambiguous", vec![]),
        };
        writeln!(
            out,
            "{}",
            serde_json::json!({"id": row["id"], "status": status, "values": values})
        )?;
    }
    Ok(())
}
