//! 选择器记忆：结构化抽取的字段在页面改版后重定位时用（0.93）。
//!
//! 键是“注册域 + schema id + 字段”，值由调用方序列化（`pagesift_extract::locate::Target`：
//! 指纹、当前选择器、可选身份属性），记录时间戳就是最近一次命中的时间。
//! 精确命中时刷新（重写同一键），重定位成功时写回新选择器。不参与缓存淘汰。

use std::time::SystemTime;

use crate::engine::{group_key, ns};
use crate::{Result, Store, to_ms};

pub struct Selectors<'a> {
    store: &'a Store,
}

/// 一条记忆：调用方的序列化值，以及最近一次命中（写入）的时间，Unix 毫秒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorEntry {
    pub value: Vec<u8>,
    pub last_hit_ms: u64,
}

fn key(site: &str, schema_id: &str, field: &str) -> Result<Vec<u8>> {
    // 组名是 `站点\0schema`，字段放在组内：同一 schema 的字段可以一起列出
    let mut group = Vec::with_capacity(site.len() + schema_id.len() + 1);
    group.extend_from_slice(site.as_bytes());
    group.push(0);
    group.extend_from_slice(schema_id.as_bytes());
    group_key(&group, field.as_bytes())
}

impl<'a> Selectors<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn get(&self, site: &str, schema_id: &str, field: &str) -> Result<Option<SelectorEntry>> {
        Ok(self
            .store
            .get(ns::SELECTOR, &key(site, schema_id, field)?)?
            .map(|(ts, value)| SelectorEntry {
                value,
                last_hit_ms: ts,
            }))
    }

    /// 写入（或刷新）一条记忆，时间戳记为现在。
    pub fn put(&self, site: &str, schema_id: &str, field: &str, value: &[u8]) -> Result<()> {
        self.store
            .put(
                ns::SELECTOR,
                &key(site, schema_id, field)?,
                value,
                to_ms(SystemTime::now()),
            )
            .map(drop)
    }

    pub fn forget(&self, site: &str, schema_id: &str, field: &str) -> Result<bool> {
        self.store
            .delete(ns::SELECTOR, &key(site, schema_id, field)?)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[test]
    fn put_get_refresh_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open_dir(dir.path()).unwrap();
            let sel = s.selectors();
            assert!(sel.get("example.com", "abc", "price").unwrap().is_none());
            sel.put("example.com", "abc", "price", b"v1").unwrap();
            sel.put("example.com", "abc", "price", b"v2").unwrap();
            sel.put("example.com", "abc", "title", b"t").unwrap();
            let e = sel.get("example.com", "abc", "price").unwrap().unwrap();
            assert_eq!(e.value, b"v2");
            assert!(e.last_hit_ms > 0);
            s.flush().unwrap();
        }
        let s = Store::open_dir(dir.path()).unwrap();
        assert_eq!(
            s.selectors()
                .get("example.com", "abc", "price")
                .unwrap()
                .unwrap()
                .value,
            b"v2"
        );
        assert!(s.selectors().forget("example.com", "abc", "price").unwrap());
        assert!(
            s.selectors()
                .get("example.org", "abc", "title")
                .unwrap()
                .is_none()
        );
    }
}
