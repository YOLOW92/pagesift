//! `pagesift models fetch`：提前下好 OCR 模型（离线环境）。

use crate::*;

pub(crate) fn run(action: ModelsCommand, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        ModelsCommand::Fetch { quality } => {
            let root = cfg.models_root()?;
            let (det, rec) = match quality.as_deref() {
                None | Some("auto") => cfg.ocr_models(),
                Some("fast") => pagesift_ocr::models::Quality::Fast.models(false),
                Some("accurate") => pagesift_ocr::models::Quality::Accurate.models(false),
                Some(other) => {
                    anyhow::bail!("--quality must be auto, fast or accurate, not {other:?}")
                }
            };
            let fetcher = cfg.scraper(false)?.fetcher;
            runtime()?.block_on(async {
                for m in [det, rec] {
                    pagesift::models::fetch(&fetcher, &root, &m, |f| {
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
