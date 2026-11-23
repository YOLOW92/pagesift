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

pub fn context(rt: &Runtime, s: &Arc<Scraper>, request_json: Option<&str>) -> Value {
    let req = match args::object(request_json) {
        Ok(m) => Value::Object(m),
        Err(e) => return failure_value(&e),
    };
    match rt.block_on(pagesift_core::loop_ops::context(s, req)) {
        Ok(v) => v,
        Err(e) => failure_value(&e),
    }
}

pub fn resolve(rt: &Runtime, s: &Arc<Scraper>, request_json: Option<&str>) -> Value {
    let req = match args::object(request_json) {
        Ok(m) => Value::Object(m),
        Err(e) => return failure_value(&e),
    };
    match rt.block_on(pagesift_core::loop_ops::resolve(s, req)) {
        Ok(v) => v,
        Err(e) => failure_value(&e),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, CString};

    use serde_json::Value;

    use crate::{
        pagesift_context, pagesift_engine_free, pagesift_engine_new, pagesift_graph_query,
        pagesift_resolve, pagesift_string_free, pagesift_sync,
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

        let ctx_ingest = CString::new(
            serde_json::json!({
                "action": "ingest",
                "source_id": "ffi-ctx",
                "documents": [{
                    "url": "https://example.com/spec",
                    "markdown": "# Spec\n\nMax payload: 64 MiB unless chunked.\n"
                }]
            })
            .to_string(),
        )
        .unwrap();
        let ingested = parse_c(unsafe { pagesift_context(engine, ctx_ingest.as_ptr()) });
        assert_eq!(ingested["success"], true);
        assert!(ingested["liveUnitCount"].as_u64().unwrap_or(0) >= 1);

        let ctx_pack = CString::new(
            r#"{"action":"pack","source_id":"ffi-ctx","query":"Max payload MiB","budget_tokens":256}"#,
        )
        .unwrap();
        let packed = parse_c(unsafe { pagesift_context(engine, ctx_pack.as_ptr()) });
        assert_eq!(packed["success"], true);
        assert!(
            packed["pack"]["compiledContext"]
                .as_str()
                .unwrap_or("")
                .contains("64 MiB")
        );

        // NULL engine, NULL request_json, invalid JSON, and out-of-range budget_tokens must all
        // safely return structured JSON errors without panicking or leaking across FFI.
        let null_engine_err =
            parse_c(unsafe { pagesift_context(std::ptr::null(), ctx_pack.as_ptr()) });
        assert_eq!(null_engine_err["success"], false);
        assert_eq!(null_engine_err["error_code"], "INVALID_REQUEST");

        let null_req_err = parse_c(unsafe { pagesift_context(engine, std::ptr::null()) });
        assert_eq!(null_req_err["success"], false);
        assert_eq!(null_req_err["error_code"], "INVALID_REQUEST");

        let bad_json = CString::new("{unclosed").unwrap();
        let bad_json_err = parse_c(unsafe { pagesift_context(engine, bad_json.as_ptr()) });
        assert_eq!(bad_json_err["success"], false);
        assert_eq!(bad_json_err["error_code"], "INVALID_REQUEST");

        let huge_budget = CString::new(
            r#"{"action":"pack","source_id":"ffi-ctx","query":"Max payload","budget_tokens":99999999}"#,
        )
        .unwrap();
        let huge_err = parse_c(unsafe { pagesift_context(engine, huge_budget.as_ptr()) });
        assert_eq!(huge_err["success"], false);
        assert_eq!(huge_err["error_code"], "INVALID_REQUEST");

        // 1.7 Resolve check (persisted) + get + NULL safety over C ABI
        let res_check = CString::new(
            serde_json::json!({
                "action": "check",
                "source_id": "ffi-ctx",
                "answer": "Max payload is 64 MiB unless chunked.",
                "persist": true
            })
            .to_string(),
        )
        .unwrap();
        let checked = parse_c(unsafe { pagesift_resolve(engine, res_check.as_ptr()) });
        assert_eq!(checked["success"], true);
        assert_eq!(checked["outcome"]["persisted"], true);
        assert_eq!(checked["outcome"]["conclusion"]["state"], "supported");
        let cid = checked["outcome"]["conclusion"]["conclusionId"]
            .as_str()
            .unwrap()
            .to_owned();

        let res_get =
            CString::new(serde_json::json!({ "action": "get", "conclusion_id": cid }).to_string())
                .unwrap();
        let got = parse_c(unsafe { pagesift_resolve(engine, res_get.as_ptr()) });
        assert_eq!(got["success"], true);
        assert_eq!(got["conclusion"]["state"], "supported");

        let null_res = parse_c(unsafe { pagesift_resolve(std::ptr::null(), res_get.as_ptr()) });
        assert_eq!(null_res["success"], false);
        assert_eq!(null_res["error_code"], "INVALID_REQUEST");

        unsafe { pagesift_string_free(std::ptr::null_mut()) };
        unsafe { pagesift_engine_free(engine) };
        unsafe { pagesift_engine_free(std::ptr::null_mut()) };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
