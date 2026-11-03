//! Identity-addressed, bounded, resumable file archive. This is a downloader,
//! not a site-extractor library. One advisory lock protects each archive root.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use pagesift_net::{FetchRequest, Url};
use pagesift_policy::{Policy, RobotsMode};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// 检查点间隔的先验（先验包 v0：0.81 的现值 1 MiB）；之后由 [`Cadence`] 按 Young–Daly 推出。
const CHECKPOINT_PRIOR: u64 = 1024 * 1024;
const MAX_STATE_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub source: String,
    pub id: String,
    /// Caller-supplied version. A changed URL alone does not change item identity.
    pub revision: String,
    pub url: Url,
    pub max_bytes: u64,
    pub expected_blake3: Option<String>,
    /// Discard an incomplete transfer, never an already completed archive item.
    pub restart: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
    pub resumed_bytes: u64,
    pub cached: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    url_key: String,
    etag: Option<String>,
    total: Option<u64>,
    bytes: u64,
    blake3: String,
    complete: bool,
    /// 这一项传输被中断（进程退出、连接断开后从检查点续传）的次数。旧版本的检查点没有这一项。
    #[serde(default)]
    interruptions: u32,
    /// 各次传输累计的时长（毫秒），和中断次数一起估计平均无故障时间（[`Cadence`]）。
    #[serde(default)]
    transfer_ms: u64,
}

pub struct Archive {
    root: PathBuf,
    _lock: File,
}

impl Archive {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let lock_path = root.join("archive.lock");
        regular_or_missing(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).context("archive already has a writer")?;
        Ok(Self { root, _lock: lock })
    }

    pub async fn download(
        &mut self,
        policy: &Policy,
        req: &DownloadRequest,
    ) -> Result<DownloadResult> {
        self.download_inner(policy, req, None).await
    }

    /// Archive exactly one inclusive byte range. Receipts are isolated by
    /// range; a server returning 200 is never sliced or appended silently.
    pub async fn download_range(
        &mut self,
        policy: &Policy,
        req: &DownloadRequest,
        start: u64,
        end: u64,
    ) -> Result<DownloadResult> {
        ensure!(
            start <= end && end.checked_add(1).is_some(),
            "invalid requested range"
        );
        ensure!(end - start < req.max_bytes, "range exceeds max_bytes");
        self.download_inner(policy, req, Some((start, end))).await
    }

    async fn download_inner(
        &mut self,
        policy: &Policy,
        req: &DownloadRequest,
        range: Option<(u64, u64)>,
    ) -> Result<DownloadResult> {
        ensure!(
            !req.source.trim().is_empty()
                && !req.id.trim().is_empty()
                && !req.revision.trim().is_empty(),
            "source, id and revision must be nonempty"
        );
        ensure!(req.max_bytes > 0, "max_bytes must be positive");
        if let Some(hash) = &req.expected_blake3 {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "expected_blake3 must be a 64-digit hex digest"
            );
        }
        // Hash a structured tuple, avoiding delimiter ambiguity and path injection.
        let identity = if let Some(range) = range {
            serde_json::to_vec(&(&req.source, &req.id, &req.revision, range))?
        } else {
            serde_json::to_vec(&(&req.source, &req.id, &req.revision))?
        };
        let key = blake3::hash(&identity).to_hex().to_string();
        let final_path = self.root.join(format!("{key}.bin"));
        let partial = self.root.join(format!("{key}.part"));
        let state_path = self.root.join(format!("{key}.json"));
        for path in [&final_path, &partial, &state_path] {
            regular_or_missing(path)?;
        }
        let url_key = blake3::hash(req.url.as_str().as_bytes())
            .to_hex()
            .to_string();
        let mut state = match read_checkpoint(&state_path)? {
            Some(state) => state,
            None => Checkpoint {
                version: 1,
                url_key: url_key.clone(),
                etag: None,
                total: None,
                bytes: 0,
                blake3: blake3::hash(b"").to_hex().to_string(),
                complete: false,
                interruptions: 0,
                transfer_ms: 0,
            },
        };
        ensure!(
            state.version == 1,
            "unsupported download checkpoint version"
        );
        ensure!(state.bytes <= req.max_bytes, "saved file exceeds max_bytes");
        if state.complete {
            let path = if final_path.exists() {
                &final_path
            } else {
                &partial
            };
            let hash = hash_prefix(path, state.bytes, true)?;
            ensure!(
                hash.clone().finalize().to_hex().as_str() == state.blake3,
                "completed archive checksum mismatch"
            );
            check_expected(req, &state.blake3)?;
            if !final_path.exists() {
                publish(&self.root, &partial, &final_path)?;
            }
            return Ok(DownloadResult {
                path: final_path,
                bytes: state.bytes,
                blake3: state.blake3,
                resumed_bytes: 0,
                cached: true,
            });
        }
        ensure!(
            !final_path.exists(),
            "uncommitted destination exists; refusing to overwrite it"
        );
        let mut hasher = if !req.restart
            && state.url_key == url_key
            && state.etag.is_some()
            && state.bytes > 0
        {
            let hash = hash_prefix(&partial, state.bytes, false)?;
            ensure!(
                hash.clone().finalize().to_hex().as_str() == state.blake3,
                "partial archive checksum mismatch; use restart to discard it"
            );
            state.interruptions = state.interruptions.saturating_add(1);
            hash
        } else {
            state = Checkpoint {
                version: 1,
                url_key,
                etag: None,
                total: None,
                bytes: 0,
                blake3: blake3::hash(b"").to_hex().to_string(),
                complete: false,
                interruptions: 0,
                transfer_ms: 0,
            };
            write_checkpoint(&self.root, &state_path, &state)?;
            blake3::Hasher::new()
        };
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&partial)
            .await?;
        // Bytes after the durable checkpoint may have been written before a crash.
        file.set_len(state.bytes).await?;
        let mut offset = state.bytes;
        let mut request = FetchRequest::new(req.url.as_str());
        if let Some((start, end)) = range {
            request = request.header("Range", format!("bytes={}-{}", start + offset, end));
            if offset > 0 {
                request = request.header("If-Range", state.etag.as_deref().unwrap());
            }
        } else if offset > 0 {
            request = request
                .header("Range", format!("bytes={offset}-"))
                .header("If-Range", state.etag.as_deref().unwrap());
        }
        let (mut response, _permit) = policy.stream_held(request, RobotsMode::Respect).await?;
        ensure!(
            matches!(response.status, 200 | 206),
            "download HTTP status {}",
            response.status
        );
        ensure!(
            !response.header("content-type").is_some_and(|v| v
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("text/html")),
            "HTML response is not a media file"
        );
        let length = response
            .header("content-length")
            .map(str::parse::<u64>)
            .transpose()
            .context("invalid Content-Length")?;
        let etag = response
            .header("etag")
            .filter(|v| v.starts_with('"') && v.ends_with('"'))
            .map(str::to_owned);
        let expected_end = if response.status == 206 {
            ensure!(
                offset > 0 || range.is_some(),
                "unsolicited partial response"
            );
            let (start, end, total) = content_range(
                response
                    .header("content-range")
                    .context("missing Content-Range")?,
            )?;
            if let Some((first, last)) = range {
                ensure!(
                    start == first + offset && end == last && (offset == 0 || etag == state.etag),
                    "range representation or bounds mismatch"
                );
                state.total = Some(last - first + 1);
                state.etag = etag;
                write_checkpoint(&self.root, &state_path, &state)?;
            } else {
                ensure!(
                    start == offset && state.total.is_none_or(|n| n == total) && etag == state.etag,
                    "resume representation or range mismatch"
                );
                state.total = Some(total);
            }
            ensure!(
                length.is_none_or(|n| n == end - start + 1),
                "range length mismatch"
            );
            Some(end + 1 - range.map_or(0, |(first, _)| first))
        } else {
            if range.is_some() {
                // A changed representation cannot replace a bounded range with
                // an entire file. Discard the prefix so an explicit retry starts
                // with a fresh Range and no stale If-Range validator.
                state.bytes = 0;
                state.etag = None;
                state.total = None;
                state.blake3 = blake3::hash(b"").to_hex().to_string();
                file.set_len(0).await?;
                write_checkpoint(&self.root, &state_path, &state)?;
                bail!("server did not honor bounded Range; prefix discarded, retry starts fresh");
            }
            // If-Range mismatch or a server ignoring Range: this response is a
            // complete replacement. Never append it to the old representation.
            offset = 0;
            hasher = blake3::Hasher::new();
            state.bytes = 0;
            state.blake3 = hasher.clone().finalize().to_hex().to_string();
            state.etag = etag;
            state.total = length;
            write_checkpoint(&self.root, &state_path, &state)?;
            file.set_len(0).await?;
            length
        };
        ensure!(
            state.total.is_none_or(|n| n <= req.max_bytes),
            "response exceeds max_bytes"
        );
        use tokio::io::AsyncSeekExt;
        file.seek(SeekFrom::Start(state.bytes)).await?;
        let mut last_checkpoint = state.bytes;
        let mut cadence = Cadence::new(state.interruptions, state.transfer_ms, state.bytes);
        let transfer: Result<()> = async {
            while let Some(chunk) = response.chunk().await? {
                let next = state
                    .bytes
                    .checked_add(chunk.len() as u64)
                    .context("file size overflow")?;
                ensure!(
                    next <= req.max_bytes && expected_end.is_none_or(|n| next <= n),
                    "response exceeds declared or allowed size"
                );
                file.write_all(&chunk).await?;
                hasher.update(&chunk);
                state.bytes = next;
                if state.bytes - last_checkpoint >= cadence.interval(state.bytes) {
                    let began = std::time::Instant::now();
                    state.transfer_ms = cadence.transfer_ms();
                    checkpoint(&self.root, &state_path, &file, &mut state, &hasher).await?;
                    cadence.checkpointed(began.elapsed().as_secs_f64(), state.bytes);
                    last_checkpoint = state.bytes;
                }
            }
            ensure!(
                expected_end.is_none_or(|n| n == state.bytes),
                "incomplete HTTP body"
            );
            ensure!(
                state.total.is_none_or(|n| n == state.bytes),
                "partial range does not complete the file"
            );
            Ok(())
        }
        .await;
        // Save only acknowledged writes, even when the peer disconnects or the
        // size guard rejects a chunk. No final file is visible on these failures.
        state.transfer_ms = cadence.transfer_ms();
        checkpoint(&self.root, &state_path, &file, &mut state, &hasher).await?;
        transfer?;
        check_expected(req, &state.blake3)?;
        file.sync_all().await?;
        drop(file);
        state.complete = true;
        // A ready receipt before publication repairs a crash between these steps.
        write_checkpoint(&self.root, &state_path, &state)?;
        publish(&self.root, &partial, &final_path)?;
        Ok(DownloadResult {
            path: final_path,
            bytes: state.bytes,
            blake3: state.blake3,
            resumed_bytes: offset,
            cached: false,
        })
    }
}

fn check_expected(req: &DownloadRequest, actual: &str) -> Result<()> {
    ensure!(
        req.expected_blake3
            .as_ref()
            .is_none_or(|v| v.eq_ignore_ascii_case(actual)),
        "download checksum does not match expected_blake3"
    );
    Ok(())
}

/// 检查点间隔：Young–Daly 的最优间隔 `τ = √(2·C·M)`（J. W. Young, CACM 17(9), 1974；
/// J. T. Daly, FGCS 22(3), 2006 的一阶项），`C` 是写一次检查点的耗时，`M` 是平均无故障时间。
/// 换成字节：间隔 = 传输速率 × τ。
///
/// - `C`：本次传输里各次检查点耗时的窗口均值（`pagesift_stat::window::WindowMean`，窗口均值的
///   相对标准误小于 `δ` 才发布）；还没有发布值时用已测的样本均值。
/// - `M`：故障按指数寿命，`M` 的估计是"累计传输时长 / 中断次数"。先验（先验包 v0：0.81 的现值）
///   是一次伪故障，取让第一次测到 `C` 和速率时 Young–Daly 正好给出 0.81 的 1 MiB 的那个 `M₀`。
///   后验 `M̂ = (M₀ + T) / (1 + k)`（`T` 是先前各次的累计传输时长，`k` 是中断次数）只在和 `M₀`
///   的差超过 `z(1−δ)·SE` 时才用，`SE = M̂/√(1+k)`（指数寿命的均值估计的标准误）。
///   没有中断时永远不显著：间隔保持先验，只有确实观察到中断才缩短。
/// - 还没有测到 `C` 时（第一个检查点之前）用先验 1 MiB。
struct Cadence {
    began: std::time::Instant,
    start_bytes: u64,
    prior_ms: u64,
    interruptions: u32,
    cost: pagesift_stat::window::WindowMean,
    cost_sum: f64,
    cost_n: f64,
    published: Option<f64>,
    mtbf0: Option<f64>,
}

impl Cadence {
    fn new(interruptions: u32, prior_ms: u64, start_bytes: u64) -> Self {
        Self {
            began: std::time::Instant::now(),
            start_bytes,
            prior_ms,
            interruptions,
            cost: Default::default(),
            cost_sum: 0.0,
            cost_n: 0.0,
            published: None,
            mtbf0: None,
        }
    }

    /// 各次传输累计的时长（毫秒），写进检查点。
    fn transfer_ms(&self) -> u64 {
        self.prior_ms
            .saturating_add(self.began.elapsed().as_millis() as u64)
    }

    fn rate(&self, bytes: u64) -> f64 {
        bytes.saturating_sub(self.start_bytes) as f64 / self.began.elapsed().as_secs_f64()
    }

    /// 写完一次检查点：耗时 `secs`，此时文件有 `bytes` 字节。
    fn checkpointed(&mut self, secs: f64, bytes: u64) {
        self.cost_sum += secs;
        self.cost_n += 1.0;
        if let Some(m) = self.cost.push(secs, pagesift_stat::Delta::DEFAULT) {
            self.published = Some(m);
        }
        if self.mtbf0.is_none() {
            let rate = self.rate(bytes);
            if rate > 0.0 && secs > 0.0 {
                let tau = CHECKPOINT_PRIOR as f64 / rate;
                // [推导] Young–Daly τ = √(2CM) 解出 M = τ² / (2C)
                self.mtbf0 = Some(tau * tau / (2.0 * secs));
            }
        }
    }

    /// 当前的检查点间隔（字节）。
    fn interval(&self, bytes: u64) -> u64 {
        let (Some(m0), true) = (self.mtbf0, self.cost_n > 0.0) else {
            return CHECKPOINT_PRIOR;
        };
        let c = self.published.unwrap_or(self.cost_sum / self.cost_n);
        let k = f64::from(self.interruptions);
        let post = (m0 + self.prior_ms as f64 / 1000.0) / (1.0 + k);
        let delta = pagesift_stat::Delta::DEFAULT;
        let m = if pagesift_stat::shrink::significant(m0, post, post / (1.0 + k).sqrt(), delta) {
            post
        } else {
            m0
        };
        let rate = self.rate(bytes);
        let bytes = rate * (2.0 * c * m).sqrt();
        if bytes.is_finite() && bytes >= 1.0 {
            bytes as u64
        } else {
            CHECKPOINT_PRIOR
        }
    }
}

async fn checkpoint(
    root: &Path,
    path: &Path,
    file: &tokio::fs::File,
    state: &mut Checkpoint,
    hash: &blake3::Hasher,
) -> Result<()> {
    file.sync_data().await?;
    state.blake3 = hash.clone().finalize().to_hex().to_string();
    write_checkpoint(root, path, state)
}

fn read_checkpoint(path: &Path) -> Result<Option<Checkpoint>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.len() <= MAX_STATE_BYTES,
        "oversized download checkpoint"
    );
    Ok(Some(serde_json::from_reader(file)?))
}

fn regular_or_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "archive path must be a regular file"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn write_checkpoint(root: &Path, path: &Path, state: &Checkpoint) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    regular_or_missing(&temp)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    serde_json::to_writer(&mut file, state)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    sync_directory(root)
}

fn publish(root: &Path, partial: &Path, destination: &Path) -> Result<()> {
    // Unlike rename(), hard_link() cannot overwrite an existing destination.
    fs::hard_link(partial, destination)?;
    sync_directory(root)?;
    fs::remove_file(partial)?;
    sync_directory(root)
}

fn sync_directory(root: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

fn hash_prefix(path: &Path, bytes: u64, exact: bool) -> Result<blake3::Hasher> {
    let mut file = File::open(path).context("missing archive payload")?;
    let len = file.metadata()?.len();
    ensure!(
        len >= bytes && (!exact || len == bytes),
        "archive payload length mismatch"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = bytes;
    let mut buf = [0u8; 64 * 1024];
    let mut hash = blake3::Hasher::new();
    while remaining > 0 {
        let limit = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..limit])?;
        ensure!(n != 0, "archive payload ended during verification");
        hash.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(hash)
}

fn content_range(value: &str) -> Result<(u64, u64, u64)> {
    let value = value
        .strip_prefix("bytes ")
        .context("invalid Content-Range unit")?;
    let (range, total) = value.split_once('/').context("invalid Content-Range")?;
    let (start, end) = range.split_once('-').context("invalid Content-Range")?;
    let (start, end, total): (u64, u64, u64) = (start.parse()?, end.parse()?, total.parse()?);
    if start > end || end >= total {
        bail!("invalid Content-Range bounds");
    }
    Ok((start, end, total))
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    /// 冷启动（还没测到检查点耗时）等于 0.81 的 1 MiB；测到以后、没有中断时仍是 1 MiB
    /// （M₀ 按它反推）；观察到多次中断后按 Young–Daly 缩短，约为 √(1/(1+k)) 倍。
    #[test]
    fn young_daly_interval() {
        let mut c = Cadence::new(0, 0, 0);
        assert_eq!(c.interval(0), CHECKPOINT_PRIOR);
        std::thread::sleep(std::time::Duration::from_millis(20));
        c.checkpointed(0.002, CHECKPOINT_PRIOR);
        let i = c.interval(CHECKPOINT_PRIOR) as f64;
        assert!((i / CHECKPOINT_PRIOR as f64 - 1.0).abs() < 0.2, "{i}");
        // 同样的耗时和速率，但先前中断过 30 次、累计传输很短：M̂ ≈ M₀/31，间隔缩到约 1/√31
        let mut d = Cadence::new(30, 0, 0);
        d.began = c.began;
        d.checkpointed(0.002, CHECKPOINT_PRIOR);
        let j = d.interval(CHECKPOINT_PRIOR) as f64;
        let ratio = j / i;
        assert!((ratio - (1.0f64 / 31.0).sqrt()).abs() < 0.05, "{ratio}");
    }
}
