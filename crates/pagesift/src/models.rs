//! 下载模型：从上游的锁定版本下载，边下边校验大小和 SHA-256，校验过才放到最终位置。走
//! `pagesift-net`，遵守代理等网络设置。权重不随本仓库分发。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig};
use pagesift_ocr::models::{File, Hasher, Model};

/// [策略] 整个文件的下载时限按这个最低速度算（字节/秒）：卡住的连接由空闲超时挡，这里只防
/// 无限拖延
const MIN_SPEED: u64 = 64 << 10;
/// [策略] 小文件的时限下限
const MIN_TIMEOUT: Duration = Duration::from_secs(60);

/// 同一进程里每个模型目录一把锁：一个进程可以有好几个引擎（Python、Node 各建一个 Scraper）。
fn model_lock(dir: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(dir.to_owned())
        .or_default()
        .clone()
}

/// 临时文件的路径：进程号区分同时下载的进程，计数区分进程内的每次下载。
fn part_path(dir: &Path, name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{name}.part.{}.{n}", std::process::id()))
}

async fn fetch_file(fetcher: &Fetcher, model: &Model, file: &File, dir: &Path) -> Result<()> {
    let url = model.url(file);
    let mut req = FetchRequest::new(url.clone());
    req.timeout = Some(MIN_TIMEOUT.max(Duration::from_secs(file.size / MIN_SPEED)));
    let mut body = fetcher
        .fetch_stream(req)
        .await
        .with_context(|| format!("downloading {url}"))?;
    // [协议] HTTP 200
    if body.status != 200 {
        bail!("downloading {url}: HTTP {}", body.status);
    }
    // 临时名各不相同，校验过才改名，别人只会看到完整的文件
    let part = part_path(dir, file.name);
    let result = async {
        let mut out =
            std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        let mut hash = Hasher::new(file);
        while let Some(chunk) = body
            .chunk()
            .await
            .with_context(|| format!("downloading {url}"))?
        {
            hash.update(&chunk);
            out.write_all(&chunk)
                .with_context(|| format!("writing {}", part.display()))?;
        }
        out.sync_all()
            .with_context(|| format!("writing {}", part.display()))?;
        hash.finish()
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("checking {url}"))?;
        let dest = dir.join(file.name);
        std::fs::rename(&part, &dest).with_context(|| format!("moving to {}", dest.display()))
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// 把模型的文件下载到 `<root>/<仓库名>`：已有且大小对的不重下，`progress` 每开始一个文件调用一次。
pub async fn fetch(
    config: &FetcherConfig,
    root: &Path,
    model: &Model,
    mut progress: impl FnMut(&File),
) -> Result<()> {
    let dir = model.dir(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let fetcher = Fetcher::new(config.clone())?;
    // 持锁下载整个模型；等到锁时别人可能已经下好了，所以持锁之后才看文件在不在
    let lock = model_lock(&dir);
    let _guard = lock.lock().await;
    for file in model.files {
        let have = std::fs::metadata(dir.join(file.name)).is_ok_and(|m| m.len() == file.size);
        if !have {
            progress(file);
            fetch_file(&fetcher, model, file, &dir).await?;
        }
    }
    Ok(())
}

/// [`fetch`] 的同步版，给解析线程用：在自己的线程和运行时里跑，不依赖调用方有没有运行时。
pub(crate) fn fetch_blocking(
    config: &FetcherConfig,
    root: &Path,
    model: &Model,
) -> Result<(), String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(fetch(config, root, model, |f| {
                tracing::info!(model = model.repo, file = f.name, "downloading OCR model");
            }))
            .map_err(|e| format!("{e:#}"))
        })
        .join()
        .unwrap_or_else(|_| Err("the model download thread panicked".into()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一进程里并发下载同一个文件时，临时名不能相同（否则互相截断，写出大小对、内容坏的权重）。
    #[test]
    fn part_paths_never_repeat() {
        let dir = Path::new("/models");
        assert_ne!(part_path(dir, "a"), part_path(dir, "a"));
    }

    #[test]
    fn one_lock_per_model_directory() {
        let (a, b) = (Path::new("/models/a"), Path::new("/models/b"));
        assert!(Arc::ptr_eq(&model_lock(a), &model_lock(a)));
        assert!(!Arc::ptr_eq(&model_lock(a), &model_lock(b)));
    }
}
