//! `ApiError` → Python 异常。异常类定义在 `pagesift/errors.py`，这里按错误码实例化。

use pagesift::api::ApiError;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList};
use serde_json::Value;

static FACTORY: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

/// 构造异常实例（批量接口把它放进 `Document.error`，不抛出）。
pub fn exception(py: Python<'_>, e: &ApiError, url: Option<&str>) -> PyResult<Py<PyAny>> {
    let factory = FACTORY.import(py, "pagesift.errors", "_from_code")?;
    let kw = PyDict::new(py);
    kw.set_item(
        "details",
        e.details.as_ref().map(|d| json(py, d)).transpose()?,
    )?;
    kw.set_item("retry_after", e.retry_after)?;
    kw.set_item("url", url)?;
    Ok(factory
        .call((e.code.as_str(), &e.message), Some(&kw))?
        .unbind())
}

pub fn to_py_err(e: &ApiError, url: Option<&str>) -> PyErr {
    Python::attach(|py| match exception(py, e, url) {
        Ok(exc) => PyErr::from_value(exc.into_bound(py)),
        Err(err) => PyRuntimeError::new_err(format!("{e} (and the error class failed: {err})")),
    })
}

pub fn json<'py>(py: Python<'py>, v: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.into_pyobject(py)?.into_any(),
            (_, Some(u)) => u.into_pyobject(py)?.into_any(),
            _ => n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any(),
        },
        Value::String(s) => s.into_pyobject(py)?.into_any(),
        Value::Array(a) => {
            let l = PyList::empty(py);
            for x in a {
                l.append(json(py, x)?)?;
            }
            l.into_any()
        }
        Value::Object(m) => {
            let d = PyDict::new(py);
            for (k, x) in m {
                d.set_item(k, json(py, x)?)?;
            }
            d.into_any()
        }
    })
}
