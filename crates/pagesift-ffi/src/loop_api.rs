//! 1.5 Loop 操作：sync 与 DataGraph 持久化查询。入参 JSON 进，结果 `Value` 出，错误不抛出。

use std::sync::Arc;

use pagesift_core::Scraper;
use serde_json::Value;
use tokio::runtime::Runtime;

use crate::args;
use crate::doc::failure_value;

pub fn sync(rt: &Runtime, s: &Arc<Scraper>, request_json: Option<&str>) -> Value {
    let req = match args::object(request_json) {
        Ok(m) => Value::Object(m),
        Err(e) => return failure_value(&e),
    };
    match rt.block_on(pagesift_core::loop_ops::sync(s, req)) {
        Ok(v) => v,
        Err(e) => failure_value(&e),
    }
}

pub fn query_graph(rt: &Runtime, s: &Arc<Scraper>, request_json: Option<&str>) -> Value {
    let req = match args::object(request_json) {
        Ok(m) => Value::Object(m),
        Err(e) => return failure_value(&e),
    };
    match rt.block_on(pagesift_core::loop_ops::query_graph(s, req)) {
        Ok(v) => v,
        Err(e) => failure_value(&e),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, CString};

    use serde_json::Value;

    use crate::{
        pagesift_engine_free, pagesift_engine_new, pagesift_graph_query, pagesift_string_free,
        pagesift_sync,
    };

    fn parse_c(ptr: *mut std::ffi::c_char) -> Value {
        assert!(!ptr.is_null());
        // SAFETY: `ptr` comes from `pagesift_*` JSON functions and is freed once immediately after.
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        unsafe { pagesift_string_free(ptr) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn ffi_sync_and_graph_query_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "pagesift-ffi-loop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_json = CString::new(
            serde_json::json!({
                "store_path": dir.to_string_lossy(),
                "allow_private_network": true
            })
            .to_string(),
        )
        .unwrap();
        let mut err = std::ptr::null_mut();
        let engine = unsafe { pagesift_engine_new(cfg_json.as_ptr(), &mut err) };
        assert!(!engine.is_null() && err.is_null());

        let create_req = CString::new(
            r#"{"action":"create","source_id":"ffi-src","seeds":["https://example.com/p"]}"#,
        )
        .unwrap();
        let created = parse_c(unsafe { pagesift_sync(engine, create_req.as_ptr()) });
        assert_eq!(created["success"], true);
        assert_eq!(created["source"]["id"], "ffi-src");

        let list_req = CString::new(r#"{"action":"list"}"#).unwrap();
        let listed = parse_c(unsafe { pagesift_sync(engine, list_req.as_ptr()) });
        assert_eq!(listed["total"], 1);

        let state_req = CString::new(r#"{"action":"state","id":"ffi-src"}"#).unwrap();
        let state = parse_c(unsafe { pagesift_sync(engine, state_req.as_ptr()) });
        assert_eq!(state["state"]["spec"]["id"], "ffi-src");

        let graph_req = CString::new(r#"{"history_url":"https://example.com/p"}"#).unwrap();
        let graph = parse_c(unsafe { pagesift_graph_query(engine, graph_req.as_ptr()) });
        assert_eq!(graph["success"], true);
        assert_eq!(graph["resourceKey"], "https://example.com/p");
        assert!(
            graph
                .get("versions")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        );

        let del_req = CString::new(r#"{"action":"delete","id":"ffi-src"}"#).unwrap();
        let deleted = parse_c(unsafe { pagesift_sync(engine, del_req.as_ptr()) });
        assert_eq!(deleted["deleted"], true);

        let missing_req = CString::new(r#"{"action":"get","id":"ffi-src"}"#).unwrap();
        let missing = parse_c(unsafe { pagesift_sync(engine, missing_req.as_ptr()) });
        assert_eq!(missing["success"], false);
        assert_eq!(missing["error_code"], "NOT_FOUND");

        unsafe { pagesift_engine_free(engine) };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
