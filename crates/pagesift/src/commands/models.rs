//! `pagesift models fetch`：提前下好 OCR 模型（离线环境）。

use crate::*;

pub(crate) fn run(action: ModelsCommand, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        ModelsCommand::Fetch { quality } => {
            let root = cfg.models_root()?;
            use pagesift_ocr::device::{Device, Quality, select_models};
            use pagesift_ocr::models::{FORMULA, LAYOUT, Model};
            let choice = match quality.as_deref() {
                None | Some("auto") => cfg.ocr_models()?,
                Some("fast") => select_models(Quality::Fast, Device::Cpu),
                Some("accurate") => select_models(Quality::Accurate, Device::Cpu),
                Some(other) => {
                    anyhow::bail!("--quality must be auto, fast or accurate, not {other:?}")
                }
            };
            let fetcher = cfg.scraper(false)?.fetcher;
            runtime()?.block_on(async {
                let math: &[Model] = if cfg.ocr.formulas {
                    &[LAYOUT, FORMULA]
                } else {
                    &[]
                };
                for m in [&choice.det, &choice.rec].into_iter().chain(math) {
                    pagesift::models::fetch(&fetcher, &root, m, |f| {
                        println!("downloading {}/{} ({} bytes)", m.repo, f.name, f.size);
                    })
                    .await?;
                    m.verify(&root).map_err(anyhow::Error::msg)?;
                    println!("ok {} in {}", m.repo, m.dir(&root).display());
                }
                Ok(ExitCode::SUCCESS)
            })
        }
    }
}
