use std::ffi::CStr;
use std::sync::LazyLock;

use pyo3::types::{PyAnyMethods, PyDict, PyFunction, PyTraceback};
use pyo3::{PyTypeInfo, prelude::*};

pub trait ToPyErr {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr;
}

#[cfg(feature = "anyhow")]
impl ToPyErr for anyhow::Error {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr {
        let err = PyErr::new::<T, _>(self.to_string());
        if let Ok(bt) = btparse::deserialize(self.backtrace())
            && let Some(tb) = to_py_traceback(&bt, py)
        {
            err.set_traceback(py, Some(tb));
        }
        err
    }
}

const CODE: &'static CStr = c"
def mk_traceback(name, filename, lineno):
    import sys
    code = compile('\\n' * (lineno - 1) + 'raise Exception()', filename, 'exec')
    code = code.replace(co_name=name)
    try:
        exec(code, dict(__name__=filename, __file__=filename), {})
    except Exception:
        return sys.exc_info()[2].tb_next
";

pub(crate) static MK_TRACEBACK: LazyLock<Py<PyFunction>> = LazyLock::new(|| {
    Python::attach(|py| {
        let locals = PyDict::new(py);
        py.run(CODE, None, Some(&locals)).expect("should run");
        locals
            .get_item("mk_traceback")
            .expect("module didn’t execute")
            .expect("mk_traceback should be defined")
            .cast_into()
            .expect("should be a function")
            .unbind()
    })
});

fn mk_traceback<'py>(
    py: Python<'py>,
    name: &str,
    filename: &str,
    lineno: usize,
) -> Bound<'py, PyTraceback> {
    MK_TRACEBACK
        .call(py, (name, filename, lineno), None)
        .expect("creating traceback failed")
        .into_bound(py)
        .extract()
        .expect("should be a traceback")
}

fn to_py_traceback<'py>(
    bt: &btparse::Backtrace,
    py: Python<'py>,
) -> Option<Bound<'py, PyTraceback>> {
    let mut tb: Option<Bound<'py, PyTraceback>> = None;
    for frame in &bt.frames {
        let tb_new = mk_traceback(
            py,
            &frame.function,
            frame.file.as_ref().map(|f| f.as_str())
                .unwrap_or("<unknown>"),
            frame.line.unwrap_or(0),
        );
        if let Some(tb) = tb {
            tb_new.setattr("tb_next", tb).expect("setattr failed");
        }
        tb = Some(tb_new);
    }
    tb
}

#[cfg(test)]
mod tests {
    use pyo3::exceptions::PyRuntimeError;
    use regex::Regex;
    use assertables::*;

    use super::*;

    fn format_exc(py: Python, py_err: PyErr) -> PyResult<String> {
        let v: Vec<String> = py
            .import("traceback")?
            .call_method1("format_exception", (&py_err,))?
            .extract()?;
        Ok(v.join(""))
    }

    #[test]
    fn anyhow() -> PyResult<()> {
        let err = anyhow::anyhow!("foo");

        Python::initialize();
        let out = Python::attach(|py| -> PyResult<String> {
            let py_err = err.to_py_err::<PyRuntimeError>(py);
            format_exc(py, py_err)
        })?;
        
        assert_is_match!(Regex::new(r#"File "[.]/src/lib.rs", line \d+, in pyo3_err_bridge::tests::anyhow"#).unwrap(), out.as_str());
        Ok(())
    }
}
