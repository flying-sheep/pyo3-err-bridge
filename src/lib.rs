use std::ffi::CStr;
use std::sync::LazyLock;

use pyo3::types::{PyAnyMethods, PyDict, PyFunction, PyTraceback};
use pyo3::{PyTypeInfo, prelude::*};

pub trait ToPyErr {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr;
}

impl<E> ToPyErr for E
where
    E: error_chain::ChainedError,
{
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr {
        let err = PyErr::new::<T, _>(self.to_string());
        if let Some(bt) = self.backtrace()
            && let Some(tb) = to_py_traceback(bt, py)
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
    lineno: u32,
) -> Bound<'py, PyTraceback> {
    MK_TRACEBACK
        .call(py, (name, filename, lineno), None)
        .expect("creating traceback failed")
        .into_bound(py)
        .extract()
        .expect("should be a traceback")
}

fn to_py_traceback<'py>(
    bt: &error_chain::Backtrace,
    py: Python<'py>,
) -> Option<Bound<'py, PyTraceback>> {
    let mut tb: Option<Bound<'py, PyTraceback>> = None;
    for frame in bt.frames() {
        let Some(sym) = frame.symbols().first() else {
            continue;
        };
        let tb_new = mk_traceback(
            py,
            sym.name()
                .map(|s| format!("{s}"))
                .unwrap_or_else(|| "<unknown>".to_string())
                .as_str(),
            sym.filename()
                .and_then(|p| p.to_str())
                .unwrap_or("<unknown>"),
            sym.lineno().unwrap_or(0),
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

    use super::*;
    
    mod test_err {
        use error_chain::error_chain;
        error_chain! {
            errors { FooError }
        }
    }

    fn format_exc(py: Python, py_err: PyErr) -> PyResult<String> {
        let v: Vec<String> = py
            .import("traceback")?
            .call_method1("format_exception", (&py_err,))?
            .extract()?;
        Ok(v.join(""))
    }

    #[test]
    fn it_works() -> PyResult<()> {
        let err = test_err::Error::from_kind(test_err::ErrorKind::FooError);
        Python::initialize();
        Python::attach(|py| -> PyResult<()> {
            let py_err = err.to_py_err::<PyRuntimeError>(py);
            let out = format_exc(py, py_err)?;
            println!("{out}");
            assert_eq!(out.as_str(), "");
            Ok(())
        })
    }
}
