//! 1.5 Loop sync & DataGraph Python bindings backed by `pagesift::loop_ops`.

use pagesift::api::ApiError;
use pyo3::prelude::*;
use serde_json::Value;

use crate::engine::{Engine, block_on};
use crate::errors::to_py_err;

fn parse_request(request_json: &str) -> PyResult<Value> {
    serde_json::from_str(request_json).map_err(|e| {
        to_py_err(
            &ApiError::invalid(format!("invalid request json: {e}")),
            None,
        )
    })
}

fn encode_result(out: Result<Value, ApiError>) -> PyResult<String> {
    let val = out.map_err(|e| to_py_err(&e, None))?;
    serde_json::to_string(&val)
        .map_err(|e| to_py_err(&ApiError::invalid(format!("serialize response: {e}")), None))
}

#[pyfunction]
pub fn engine_sync(py: Python<'_>, engine: &Engine, request_json: String) -> PyResult<String> {
    let req = parse_request(&request_json)?;
    let scraper = engine.scraper.clone();
    let out = block_on(
        py,
        async move { pagesift::loop_ops::sync(&scraper, req).await },
    )?;
    encode_result(out)
}

#[pyfunction]
pub fn aengine_sync<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let req = parse_request(&request_json)?;
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        encode_result(pagesift::loop_ops::sync(&scraper, req).await)
    })
}

#[pyfunction]
pub fn engine_query_graph(
    py: Python<'_>,
    engine: &Engine,
    request_json: String,
) -> PyResult<String> {
    let req = parse_request(&request_json)?;
    let scraper = engine.scraper.clone();
    let out = block_on(py, async move {
        pagesift::loop_ops::query_graph(&scraper, req).await
    })?;
    encode_result(out)
}

#[pyfunction]
pub fn aengine_query_graph<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let req = parse_request(&request_json)?;
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        encode_result(pagesift::loop_ops::query_graph(&scraper, req).await)
    })
}
