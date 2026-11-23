//! 1.5 Loop sync & DataGraph Python bindings backed by `pagesift::loop_ops`.

use pagesift::api::ApiError;
use pyo3::prelude::*;
use serde_json::Value;

use crate::engine::{Engine, block_on};
use crate::errors::to_py_err;

fn parse_request_raw(request_json: &str) -> Result<Value, ApiError> {
    serde_json::from_str(request_json)
        .map_err(|e| ApiError::invalid(format!("invalid request json: {e}")))
}

fn encode_value_raw(val: Value) -> Result<String, ApiError> {
    serde_json::to_string(&val).map_err(|e| ApiError::invalid(format!("serialize response: {e}")))
}

#[pyfunction]
pub fn engine_sync(py: Python<'_>, engine: &Engine, request_json: String) -> PyResult<String> {
    let scraper = engine.scraper.clone();
    let res = block_on(py, async move {
        let req = parse_request_raw(&request_json)?;
        let val = pagesift::loop_ops::sync(&scraper, req).await?;
        encode_value_raw(val)
    })?;
    res.map_err(|e| to_py_err(&e, None))
}

#[pyfunction]
pub fn aengine_sync<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let req = parse_request_raw(&request_json).map_err(|e| to_py_err(&e, None))?;
        let val = pagesift::loop_ops::sync(&scraper, req)
            .await
            .map_err(|e| to_py_err(&e, None))?;
        encode_value_raw(val).map_err(|e| to_py_err(&e, None))
    })
}

#[pyfunction]
pub fn engine_query_graph(
    py: Python<'_>,
    engine: &Engine,
    request_json: String,
) -> PyResult<String> {
    let scraper = engine.scraper.clone();
    let res = block_on(py, async move {
        let req = parse_request_raw(&request_json)?;
        let val = pagesift::loop_ops::query_graph(&scraper, req).await?;
        encode_value_raw(val)
    })?;
    res.map_err(|e| to_py_err(&e, None))
}

#[pyfunction]
pub fn aengine_query_graph<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let req = parse_request_raw(&request_json).map_err(|e| to_py_err(&e, None))?;
        let val = pagesift::loop_ops::query_graph(&scraper, req)
            .await
            .map_err(|e| to_py_err(&e, None))?;
        encode_value_raw(val).map_err(|e| to_py_err(&e, None))
    })
}

#[pyfunction]
pub fn engine_context(py: Python<'_>, engine: &Engine, request_json: String) -> PyResult<String> {
    let scraper = engine.scraper.clone();
    let res = block_on(py, async move {
        let req = parse_request_raw(&request_json)?;
        let val = pagesift::loop_ops::context(&scraper, req).await?;
        encode_value_raw(val)
    })?;
    res.map_err(|e| to_py_err(&e, None))
}

#[pyfunction]
pub fn aengine_context<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let req = parse_request_raw(&request_json).map_err(|e| to_py_err(&e, None))?;
        let val = pagesift::loop_ops::context(&scraper, req)
            .await
            .map_err(|e| to_py_err(&e, None))?;
        encode_value_raw(val).map_err(|e| to_py_err(&e, None))
    })
}

#[pyfunction]
pub fn engine_resolve(py: Python<'_>, engine: &Engine, request_json: String) -> PyResult<String> {
    let scraper = engine.scraper.clone();
    let res = block_on(py, async move {
        let req = parse_request_raw(&request_json)?;
        let val = pagesift::loop_ops::resolve(&scraper, req).await?;
        encode_value_raw(val)
    })?;
    res.map_err(|e| to_py_err(&e, None))
}

#[pyfunction]
pub fn aengine_resolve<'py>(
    py: Python<'py>,
    engine: &Engine,
    request_json: String,
) -> PyResult<Bound<'py, PyAny>> {
    let scraper = engine.scraper.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let req = parse_request_raw(&request_json).map_err(|e| to_py_err(&e, None))?;
        let val = pagesift::loop_ops::resolve(&scraper, req)
            .await
            .map_err(|e| to_py_err(&e, None))?;
        encode_value_raw(val).map_err(|e| to_py_err(&e, None))
    })
}
