//! 模型清单：从哪下载、锁定的上游版本、每个文件的大小和 SHA-256；本地缓存目录与校验。
//! 权重是 PaddlePaddle 在 Hugging Face 上发布的 PP-OCRv6 safetensors（Apache-2.0），不随本仓库
//! 分发，打开 OCR 后第一次用时下载（下载由调用方用自己的 HTTP 客户端做，这里只给地址和校验）。

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 一个模型文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File {
    pub name: &'static str,
    pub size: u64,
    /// 小写十六进制的 SHA-256。
    pub sha256: &'static str,
}

/// 一个模型（一个 Hugging Face 仓库的一个锁定版本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// 仓库名，也是本地缓存里的目录名。
    pub repo: &'static str,
    /// 锁定的提交号：内容固定，校验值才有意义。
    pub revision: &'static str,
    pub files: &'static [File],
}

/// [协议] Hugging Face 的组织名
const ORG: &str = "PaddlePaddle";

impl Model {
    /// 文件的下载地址（`https://huggingface.co/<组织>/<仓库>/resolve/<提交号>/<文件>`）。
    pub fn url(&self, file: &File) -> String {
        format!(
            "https://huggingface.co/{ORG}/{}/resolve/{}/{}",
            self.repo, self.revision, file.name
        )
    }

    /// 本地目录：`<缓存根>/<仓库名>`。
    pub fn dir(&self, root: &Path) -> PathBuf {
        root.join(self.repo)
    }

    /// 所有文件都在且大小对（不算哈希，载入模型前用）。
    pub fn present(&self, root: &Path) -> bool {
        let dir = self.dir(root);
        self.files.iter().all(|f| {
            std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size)
        })
    }

    /// 本地目录里的文件是否齐全、大小和 SHA-256 是否都对；返回第一处问题。
    pub fn verify(&self, root: &Path) -> Result<(), String> {
        let dir = self.dir(root);
        for f in self.files {
            let p = dir.join(f.name);
            let mut hash = Hasher::new(f);
            let mut file = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                let n = file
                    .read(&mut buf)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
            }
            hash.finish().map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(())
    }
}

/// [策略] 校验时每次读的字节数
const READ_CHUNK: usize = 1 << 20;

/// 边下载边校验一个文件（模型文件上百 MB，不整个放进内存）。
pub struct Hasher<'a> {
    file: &'a File,
    sha: Sha256,
    size: u64,
}

impl<'a> Hasher<'a> {
    pub fn new(file: &'a File) -> Self {
        Self {
            file,
            sha: Sha256::new(),
            size: 0, // [协议] 上游文件大小
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.sha.update(bytes);
        self.size += bytes.len() as u64;
    }

    /// 大小和 SHA-256 都对才算过；返回问题描述。
    pub fn finish(self) -> Result<(), String> {
        if self.size != self.file.size {
            return Err(format!("{} bytes, expected {}", self.size, self.file.size));
        }
        let got: String = self
            .sha
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if got != self.file.sha256 {
            return Err(format!("SHA-256 is {got}, expected {}", self.file.sha256));
        }
        Ok(())
    }
}

/// 校验下载来的一个文件。
pub fn check(f: &File, bytes: &[u8]) -> Result<(), String> {
    let mut h = Hasher::new(f);
    h.update(bytes);
    h.finish()
}

/// 缓存根目录：`$XDG_CACHE_HOME/pagesift/models`，没设时 `~/.cache/pagesift/models`
/// （XDG Base Directory 规范）。
pub fn cache_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("pagesift").join("models"))
}

// [协议] 以下大小和 SHA-256 取自各仓库锁定提交的文件（2026-11-03 核对：safetensors 的值与
// Hugging Face LFS 指针一致，JSON 文件按下载内容计算）

/// CPU 用的小模型：检测。
pub const SMALL_DET: Model = Model {
    repo: "PP-OCRv6_small_det_safetensors",
    revision: "eae2ee920a39fb3087637d3dbb58df1896ec1f24",
    files: &[
        File {
            name: "model.safetensors",
            size: 9_938_124, // [协议] 上游文件大小
            sha256: "89a96a8adc4e9cd0c994098edc76022e496d35844392562b4694c8fbc583f2da",
        },
        File {
            name: "config.json",
            size: 1_096, // [协议] 上游文件大小
            sha256: "b42d76ab6325d5234e1afc9987b73dc592b0ec8437c8a29d46ba6209b79252eb",
        },
        File {
            name: "preprocessor_config.json",
            size: 838, // [协议] 上游文件大小
            sha256: "74421569a28a78d417db320f9bc039ce0997a1defe2b35f259dfc74299c9f1ed",
        },
    ],
};

/// CPU 用的小模型：识别。
pub const SMALL_REC: Model = Model {
    repo: "PP-OCRv6_small_rec_safetensors",
    revision: "fe049fb103f57443fe8840c54ed06b702f3c1de5",
    files: &[
        File {
            name: "model.safetensors",
            size: 21_204_736, // [协议] 上游文件大小
            sha256: "f65a332afe5aa663f0b9d5706f4ae8457b5b4058a842d5c1eb22df505c27d642",
        },
        File {
            name: "config.json",
            size: 1_083, // [协议] 上游文件大小
            sha256: "8693fd8485e8543e13d0f6dde3891a3f4af9a47fc4e3a391fbed71f8015c899e",
        },
        File {
            name: "preprocessor_config.json",
            size: 206_177, // [协议] 上游文件大小
            sha256: "2b24fa36f548893f26a931cf44f1a1a6b2b14ed7e2f8f8e28e6848801a8278db",
        },
    ],
};

/// GPU 用的中模型：检测。
pub const MEDIUM_DET: Model = Model {
    repo: "PP-OCRv6_medium_det_safetensors",
    revision: "4236c2b61741a259c091fd879dcc4edc339e916c",
    files: &[
        File {
            name: "model.safetensors",
            size: 88_020_412, // [协议] 上游文件大小
            sha256: "bd393266c02e1a680b1b34c301d5d0d81e6290440b7f8ab0f5d5032276b17eb1",
        },
        File {
            name: "config.json",
            size: 2_134, // [协议] 上游文件大小
            sha256: "3ea7e760f64255c152ba139e2f5f798303f7189b87c923cf498e2b2e830cadd3",
        },
        File {
            name: "preprocessor_config.json",
            size: 838, // [协议] 上游文件大小
            sha256: "74421569a28a78d417db320f9bc039ce0997a1defe2b35f259dfc74299c9f1ed",
        },
    ],
};

/// GPU 用的中模型：识别。
pub const MEDIUM_REC: Model = Model {
    repo: "PP-OCRv6_medium_rec_safetensors",
    revision: "024cad6a831de75c2c3c26e711ba8c4a82ccd24b",
    files: &[
        File {
            name: "model.safetensors",
            size: 76_741_720, // [协议] 上游文件大小
            sha256: "5f43c16f2a684b1d2284662178bdb604febd3d6bfdb5ca73828d08d0f7c0c3e9",
        },
        File {
            name: "config.json",
            size: 1_131, // [协议] 上游文件大小
            sha256: "aa5f351b4a7df95cc4953f013d53a6d44205f1bb0aee0ec7eb243d1d8fae1c27",
        },
        File {
            name: "preprocessor_config.json",
            size: 206_177, // [协议] 上游文件大小
            sha256: "2b24fa36f548893f26a931cf44f1a1a6b2b14ed7e2f8f8e28e6848801a8278db",
        },
    ],
};

/// 公式识别（只有 L 级有 safetensors）。
pub const FORMULA: Model = Model {
    repo: "PP-FormulaNet_plus-L_safetensors",
    revision: "6cc004a8e91ea3f3819b05317f40b290cbe99c2b",
    files: &[
        File {
            name: "model.safetensors",
            size: 727_633_048, // [协议] 上游文件大小
            sha256: "582848cf9b7066bb1cf3166ad75a476da492c3661ffa61bb7439f9fd098bde13",
        },
        File {
            name: "config.json",
            size: 1_364, // [协议] 上游文件大小
            sha256: "3d5c513a525ebef73ba4e319e65de793d95aacc1062e5ce2dd851c7f109c060c",
        },
        File {
            name: "generation_config.json",
            size: 186, // [协议] 上游文件大小
            sha256: "e0dc49736b0b06f19cb2074201a072cee23707b7abff4a55a24da19b14abd707",
        },
        File {
            name: "processor_config.json",
            size: 584, // [协议] 上游文件大小
            sha256: "f1454999fcb5930c2d93e9dcd03f1adf046245e98e96a7f601dabc6ee01c3b78",
        },
        File {
            name: "tokenizer.json",
            size: 2_140_014, // [协议] 上游文件大小
            sha256: "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2",
        },
        File {
            name: "tokenizer_config.json",
            size: 4_484, // [协议] 上游文件大小
            sha256: "0bf3cca5ff1a240004932dbab09373b0e8a651f5f85f2ff6f2f9dec3f0e41947",
        },
    ],
};

/// 版面检测（找公式区域）。
pub const LAYOUT: Model = Model {
    repo: "PP-DocLayoutV3_safetensors",
    revision: "e4f489dc536556fc1be02b973ba06756e0d4a2ac",
    files: &[
        File {
            name: "model.safetensors",
            size: 133_270_468, // [协议] 上游文件大小
            sha256: "5ea422c6cc5fe759a47e1357c35639b58173508e025a3131cbe4b6ac59e2b85e",
        },
        File {
            name: "config.json",
            size: 2_460, // [协议] 上游文件大小
            sha256: "3cf834b91d23a756b1519bce4db42c09e852f3e35c35092dd5a3e253a50c071a",
        },
        File {
            name: "inference.yml",
            size: 1_482, // [协议] 上游文件大小
            sha256: "506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc",
        },
        File {
            name: "preprocessor_config.json",
            size: 575, // [协议] 上游文件大小
            sha256: "519fe0187a43a1ca429e3ad8317bab8700f0d5e8fb3a6e3a0a413ffac078ba42",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机缓存里已有的模型要能通过校验（没有缓存时跳过）。
    #[test]
    fn cached_models_verify() {
        let Some(root) = cache_root() else { return };
        for m in [
            SMALL_DET, SMALL_REC, MEDIUM_DET, MEDIUM_REC, FORMULA, LAYOUT,
        ] {
            if m.dir(&root).join("model.safetensors").exists() {
                m.verify(&root).unwrap();
            }
        }
    }

    #[test]
    fn rejects_wrong_bytes() {
        let f = &SMALL_DET.files[1];
        assert!(check(f, b"{}").is_err());
    }
}
