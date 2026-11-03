//! 工人认证：每个工人一个预共享令牌，网关只存令牌的哈希，可单独吊销。
//!
//! 令牌由部署者随机生成（高熵），所以哈希用 BLAKE3 的派生密钥模式加域分隔即可，
//! 不需要口令哈希那种故意变慢的函数；比较用 `blake3::Hash` 的常数时间相等。

use std::collections::HashMap;
use std::sync::RwLock;

use crate::types::WorkerId;

/// [协议] BLAKE3 派生密钥模式的上下文串（域分隔），改了就要重新登记全部令牌
const CONTEXT: &str = "pagesift-cluster 2026-11 worker token v1";

/// 令牌的哈希（可以写进配置文件，代替明文令牌）。
pub fn token_hash(token: &str) -> blake3::Hash {
    blake3::Hash::from(blake3::derive_key(CONTEXT, token.as_bytes()))
}

#[derive(Debug, Default)]
pub struct TokenStore {
    hashes: RwLock<HashMap<WorkerId, blake3::Hash>>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个工人的令牌（只保留哈希）。
    pub fn add(&self, worker: WorkerId, token: &str) {
        self.add_hash(worker, token_hash(token));
    }

    /// 登记哈希（配置里只写哈希的十六进制时用 `blake3::Hash::from_hex` 解析）。
    pub fn add_hash(&self, worker: WorkerId, hash: blake3::Hash) {
        self.hashes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(worker, hash);
    }

    /// 吊销；已建立的连接由调用方断开。
    pub fn revoke(&self, worker: &WorkerId) -> bool {
        self.hashes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(worker)
            .is_some()
    }

    pub fn verify(&self, worker: &WorkerId, token: &str) -> bool {
        let want = self
            .hashes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(worker)
            .copied();
        // 未登记的工人也算一次哈希，响应时间不暴露工人 id 是否存在
        let got = token_hash(token);
        want.is_some_and(|w| w == got)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_and_revoke() {
        let s = TokenStore::new();
        let w = WorkerId::new("w1");
        s.add(w.clone(), "secret");
        assert!(s.verify(&w, "secret"));
        assert!(!s.verify(&w, "secret "));
        assert!(!s.verify(&WorkerId::new("w2"), "secret"));
        let s2 = TokenStore::new();
        s2.add_hash(
            w.clone(),
            blake3::Hash::from_hex(token_hash("secret").to_hex().as_str()).unwrap(),
        );
        assert!(s2.verify(&w, "secret"));
        assert!(s.revoke(&w));
        assert!(!s.verify(&w, "secret"));
    }
}
