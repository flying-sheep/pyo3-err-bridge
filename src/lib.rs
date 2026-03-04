#![warn(clippy::pedantic)]

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
        self.backtrace().maybe_set_bt(py, &err);
        err
    }
}

#[cfg(feature = "color-eyre")]
impl ToPyErr for eyre::Report {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr {
        let err = PyErr::new::<T, _>(self.to_string());
        if let Some(bt) = self.handler().downcast_ref::<color_eyre::Handler>().and_then(|h| h.backtrace()) {
            bt.maybe_set_bt(py, &err);
        }
        err
    }
}

trait Backtrace {
    fn maybe_set_bt(&self, py: Python, err: &PyErr) {
        if let Some(tb) = self.to_py_traceback(py) {
            err.set_traceback(py, Some(tb));
        }
    }
    fn to_py_traceback<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyTraceback>>;
}

#[cfg(feature = "anyhow")]
impl Backtrace for std::backtrace::Backtrace {
    fn to_py_traceback<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyTraceback>> {
        btparse::deserialize(self).ok().and_then(|bt| bt.to_py_traceback(py))
    }
}

#[cfg(feature = "anyhow")]
impl Backtrace for btparse::Backtrace {
    fn to_py_traceback<'py>(
        &self,
        py: Python<'py>,
    ) -> Option<Bound<'py, PyTraceback>> {
        let mut tb: Option<Bound<'py, PyTraceback>> = None;
        for frame in &self.frames {
            let tb_new = mk_traceback(
                py,
                &frame.function,
                frame.file.as_ref().map_or("<unknown>", |f| f.as_str()),
                frame.line.unwrap_or(0),
            );
            if let Some(tb) = tb {
                tb_new.setattr("tb_next", tb).expect("setattr failed");
            }
            tb = Some(tb_new);
        }
        tb
    }
}

#[cfg(feature = "color-eyre")]
impl Backtrace for backtrace::Backtrace {
    fn to_py_traceback<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyTraceback>> {
        let mut tb: Option<Bound<'py, PyTraceback>> = None;
        for frame in self.frames() {
            let Some(sym) = frame.symbols().first() else { continue };
            let func_name = sym.name().map(|n| n.to_string());
            let tb_new = mk_traceback(
                py,
                func_name.as_deref().unwrap_or("<unknown function>"),
                sym.filename()
                    .and_then(|f| f.to_str())
                    .unwrap_or("<unknown>"),
                sym.lineno().map_or(0, |n| n as usize),
            );
            if let Some(tb) = tb {
                tb_new.setattr("tb_next", tb).expect("setattr failed");
            }
            tb = Some(tb_new);
        }
        tb
    }
}

const CODE: &CStr = c"
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

#[cfg(test)]
mod tests {
    use pyo3::exceptions::PyRuntimeError;
    use regex::Regex;
    use assertables::*;

    use super::*;

    fn format_exc(py: Python, py_err: &PyErr) -> PyResult<String> {
        let v: Vec<String> = py
            .import("traceback")?
            .call_method1("format_exception", (&py_err,))?
            .extract()?;
        Ok(v.join(""))
    }

    #[test]
    #[cfg(feature = "anyhow")]
    fn anyhow() -> PyResult<()> {
        let err = anyhow::anyhow!("foo");

        Python::initialize();
        let out = Python::attach(|py| -> PyResult<String> {
            let py_err = err.to_py_err::<PyRuntimeError>(py);
            format_exc(py, &py_err)
        })?;
        
        assert_is_match!(Regex::new(r#"File "[.]/src/lib.rs", line \d+, in pyo3_err_bridge::tests::anyhow"#).unwrap(), out.as_str());
        Ok(())
    }

    #[test]
    #[cfg(feature = "color-eyre")]
    fn color_eyre() -> PyResult<()> {
        color_eyre::install().unwrap();
        let err = color_eyre::eyre::eyre!("foo");

        Python::initialize();
        let out = Python::attach(|py| -> PyResult<String> {
            let py_err = err.to_py_err::<PyRuntimeError>(py);
            format_exc(py, &py_err)
        })?;
        
        assert_is_match!(Regex::new(r#"File ".+pyo3-err-bridge/src/lib.rs", line \d+, in pyo3_err_bridge\[.*\]::tests::color_eyre"#).unwrap(), out.as_str());
        Ok(())
    }
}
