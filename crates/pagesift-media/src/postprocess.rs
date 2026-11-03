//! Deterministic filenames and recoverable binary + metadata publication. The
//! sidecar is the commit marker, published last; this is not a two-file atomic FS.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::download::DownloadResult;
use crate::media::MediaItem;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
#[derive(Debug, Serialize)]
pub struct PublishedMedia {
    pub path: PathBuf,
    pub metadata_path: PathBuf,
    pub reused: bool,
}

pub fn media_filename(template: &str, item: &MediaItem, hash: &str) -> Result<String> {
    ensure!(
        template.len() <= 512
            && !template.contains(['/', '\\'])
            && !template.chars().any(char::is_control),
        "invalid filename template"
    );
    let mut out = String::new();
    let mut rest = template;
    while let Some((prefix, tail)) = rest.split_once('{') {
        ensure!(!prefix.contains('}'), "unmatched template brace");
        out.push_str(prefix);
        let (key, next) = tail
            .split_once('}')
            .context("unclosed template placeholder")?;
        let value = match key {
            "id" => &item.id,
            "title" => &item.title,
            "source" => &item.source,
            "ext" => &item.extension,
            "hash" => hash,
            _ => anyhow::bail!("unknown filename placeholder"),
        };
        out.push_str(&component(value));
        rest = next;
    }
    ensure!(!rest.contains('}'), "unmatched template brace");
    out.push_str(rest);
    let name = component(&out);
    ensure!(
        !name.is_empty() && name != "_",
        "filename is empty after sanitization"
    );
    Ok(name)
}

fn component(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        let c = if c.is_control()
            || matches!(
                c,
                '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\u{202e}' | '\u{202d}'
            ) {
            '_'
        } else {
            c
        };
        if out.len() + c.len_utf8() > 160 {
            break;
        }
        out.push(c);
    }
    let mut out = out.trim_matches(['.', ' ']).to_owned();
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

pub fn publish_media(
    item: &MediaItem,
    download: &DownloadResult,
    directory: &Path,
    template: &str,
) -> Result<PublishedMedia> {
    let name = media_filename(template, item, &download.blake3)?;
    ensure!(
        download.blake3.len() == 64 && download.blake3.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid download digest"
    );
    let metadata = serde_json::to_vec_pretty(
        &serde_json::json!({"version":1,"item":item,"bytes":download.bytes,"blake3":download.blake3,"filename":name}),
    )?;
    ensure!(metadata.len() <= 64 * 1024, "media sidecar exceeds 64 KiB");
    fs::create_dir_all(directory)?;
    let lock_path = directory.join(".postprocess.lock");
    regular(&lock_path)?;
    let lock = private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("postprocessor already has a writer")?;
    let path = directory.join(&name);
    let metadata_path = directory.join(format!("{name}.metadata.json"));
    regular(&path)?;
    regular(&metadata_path)?;
    regular(&download.path)?;
    if metadata_path.exists() {
        ensure!(
            fs::metadata(&metadata_path)?.len() <= 64 * 1024,
            "existing sidecar too large"
        );
        ensure!(
            fs::read(&metadata_path)? == metadata,
            "filename belongs to another item/revision"
        );
        check_file(&path, download)?;
        return Ok(PublishedMedia {
            path,
            metadata_path,
            reused: true,
        });
    }
    let reused = path.exists();
    if reused {
        check_file(&path, download)?;
    } else {
        let (mut file, temp) = temporary(directory)?;
        let mut input = File::open(&download.path)?;
        let mut hash = blake3::Hasher::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            ensure!(bytes <= download.bytes, "archive grew during publication");
            hash.update(&buffer[..n]);
            file.write_all(&buffer[..n])?;
        }
        ensure!(
            bytes == download.bytes && hash.finalize().to_hex().as_str() == download.blake3,
            "archive checksum mismatch"
        );
        file.sync_all()?;
        fs::hard_link(&temp.0, &path).context("refusing to overwrite exported media")?;
    }
    let (mut file, temp) = temporary(directory)?;
    file.write_all(&metadata)?;
    file.sync_all()?;
    fs::hard_link(&temp.0, &metadata_path).context("refusing to overwrite metadata")?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(PublishedMedia {
        path,
        metadata_path,
        reused,
    })
}
fn check_file(path: &Path, download: &DownloadResult) -> Result<()> {
    ensure!(
        fs::metadata(path)?.len() == download.bytes,
        "published media size mismatch"
    );
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        hash.finalize().to_hex().as_str() == download.blake3,
        "published media checksum mismatch"
    );
    Ok(())
}
fn regular(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.file_type().is_file(),
            "media publication paths must be regular files"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn temporary(root: &Path) -> Result<(File, Temporary)> {
    for _ in 0..32 {
        let path = root.join(format!(
            ".pagesift-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match private_options().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, Temporary(path))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("cannot reserve media temporary file")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path) -> (MediaItem, DownloadResult) {
        let path = root.join("archive.bin");
        fs::write(&path, b"payload").unwrap();
        let item = MediaItem {
            source: "test".into(),
            id: "42".into(),
            revision: "v1".into(),
            url: "https://example.test/a".into(),
            title: "../CON".into(),
            extension: "bin".into(),
            metadata: serde_json::json!({}),
        };
        let result = DownloadResult {
            path,
            bytes: 7,
            blake3: blake3::hash(b"payload").to_hex().to_string(),
            resumed_bytes: 0,
            cached: false,
        };
        (item, result)
    }
    #[test]
    fn publication_is_idempotent_and_rejects_collisions_and_corruption() {
        let root = tempfile::tempdir().unwrap();
        let (item, result) = fixture(root.path());
        let dest = root.path().join("out");
        let p = publish_media(&item, &result, &dest, "{id}.{ext}").unwrap();
        assert!(!p.reused);
        assert!(
            publish_media(&item, &result, &dest, "{id}.{ext}")
                .unwrap()
                .reused
        );
        let mut other = item.clone();
        other.revision = "v2".into();
        assert!(publish_media(&other, &result, &dest, "{id}.{ext}").is_err());
        fs::write(&p.path, b"changed").unwrap();
        assert!(publish_media(&item, &result, &dest, "{id}.{ext}").is_err());
    }
    #[test]
    fn recovery_after_binary_commit_and_path_safety() {
        let root = tempfile::tempdir().unwrap();
        let (item, result) = fixture(root.path());
        let dest = root.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("42.bin"), b"payload").unwrap();
        assert!(
            publish_media(&item, &result, &dest, "{id}.{ext}")
                .unwrap()
                .reused
        );
        assert!(media_filename("../{id}", &item, &result.blake3).is_err());
        assert!(media_filename("{unknown}", &item, &result.blake3).is_err());
        assert_eq!(component("CON.txt"), "_CON.txt");
        assert!(
            !media_filename("{title}", &item, &result.blake3)
                .unwrap()
                .contains('/')
        );
        #[cfg(unix)]
        {
            let root2 = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(&result.path, root2.path().join("42.bin")).unwrap();
            assert!(publish_media(&item, &result, root2.path(), "{id}.{ext}").is_err());
        }
    }
}
