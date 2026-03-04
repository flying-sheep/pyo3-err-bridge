#![warn(clippy::pedantic)]
//! Convert Errors with backtraces to Python Exceptions
//!
//! Implement [`ToPyErr`] on Error types.
//! This is usually done by calling [`ToString::to_string`] for the message and then calling [`pyo3::PyErr::set_traceback`].
//!
//! To convert the errors’ various backtraces, implement [`Backtrace`] for the type.
//! To make this easier, [`BacktraceFromFrames`] can be implemented instead.
//!
//! To create  actual Python Traceback objects (e.g. in [`BacktraceFromFrames::frame_to_py_traceback`]),
//! you can create a stackless Python traceback object with [`mk_traceback`].
//! The stack is created by setting the `tb_next` attribute on it.
//!
//! The [`ToPyErr`] trait is implemented for the following Error types:
//! - [`anyhow::Error`] (when the `anyhow` feature is enabled)
//! - [`eyre::Report`] (when the `color-eyre` feature is enabled)

use std::ffi::CStr;
use std::sync::LazyLock;

use pyo3::exceptions::PyRuntimeError;
use pyo3::types::{PyAnyMethods, PyDict, PyFunction, PyTraceback};
use pyo3::{PyTypeInfo, prelude::*};

/// Convert Errors with backtraces to Python Exceptions.
pub trait ToPyErr {
    /// Convert the Error to a Python Exception.
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr;
    /// If the backtrace can be converted via [`Backtrace::to_py`],
    /// set it on the Python Exception via [`PyErr::set_traceback`].
    fn maybe_set_backtrace<BT: Backtrace>(&self, py: Python, err: &PyErr, bt: &BT) {
        if let Ok(tb) = bt.to_py(py) {
            err.set_traceback(py, Some(tb));
        }
    }
}

#[cfg(feature = "anyhow")]
impl ToPyErr for anyhow::Error {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr {
        let err = PyErr::new::<T, _>(self.to_string());
        self.maybe_set_backtrace(py, &err, self.backtrace());
        err
    }
}

#[cfg(feature = "color-eyre")]
impl ToPyErr for eyre::Report {
    fn to_py_err<T: PyTypeInfo>(self, py: Python) -> PyErr {
        let err = PyErr::new::<T, _>(self.to_string());
        if let Some(bt) = self
            .handler()
            .downcast_ref::<color_eyre::Handler>()
            .and_then(|h| h.backtrace())
        {
            self.maybe_set_backtrace(py, &err, bt);
        }
        err
    }
}

/// A backtrace type that can be converted into a Python traceback.
pub trait Backtrace {
    /// Get a Python traceback object.
    ///
    /// # Errors
    /// Returns an error if the backtrace could not be converted.
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTraceback>>;
}

/// Backtrace implementation for types that can be iterated over.
pub trait BacktraceFromFrames: Backtrace {
    /// The frame type.
    type Frame;
    /// Get an iterator over the frames.
    fn iter_frames(&self) -> impl Iterator<Item = &Self::Frame>;
    /// Convert a frame into a Python traceback object.
    /// By default this uses [`mk_traceback`].
    ///
    /// # Errors
    /// Returns an error if the frame could not be converted.
    fn frame_to_py_traceback<'py>(
        &self,
        py: Python<'py>,
        frame: &Self::Frame,
    ) -> PyResult<Bound<'py, PyTraceback>>;
}

impl<B> Backtrace for B
where
    B: BacktraceFromFrames,
{
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTraceback>> {
        let mut tb: PyResult<Bound<'py, PyTraceback>> =
            Err(PyErr::new::<PyRuntimeError, _>("no frames"));
        for frame in self.iter_frames() {
            let tb_new = self.frame_to_py_traceback(py, frame)?;
            if let Ok(tb) = tb {
                tb_new.setattr("tb_next", tb)?;
            }
            tb = Ok(tb_new);
        }
        tb
    }
}

#[cfg(feature = "std")]
impl Backtrace for std::backtrace::Backtrace {
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTraceback>> {
        btparse::deserialize(self)
            .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?
            .to_py(py)
    }
}

#[cfg(feature = "std")]
impl BacktraceFromFrames for btparse::Backtrace {
    type Frame = btparse::Frame;
    fn iter_frames(&self) -> impl Iterator<Item = &Self::Frame> {
        self.frames.iter()
    }
    fn frame_to_py_traceback<'py>(
        &self,
        py: Python<'py>,
        frame: &Self::Frame,
    ) -> PyResult<Bound<'py, PyTraceback>> {
        mk_traceback(
            py,
            &frame.function,
            frame.file.as_ref().map_or("<unknown>", |f| f.as_str()),
            frame.line.unwrap_or(0),
        )
    }
}

#[cfg(feature = "color-eyre")]
impl BacktraceFromFrames for backtrace::Backtrace {
    type Frame = backtrace::BacktraceSymbol;
    fn iter_frames(&self) -> impl Iterator<Item = &Self::Frame> {
        self.frames().iter().filter_map(|f| f.symbols().first())
    }
    fn frame_to_py_traceback<'py>(
        &self,
        py: Python<'py>,
        frame: &Self::Frame,
    ) -> PyResult<Bound<'py, PyTraceback>> {
        let func_name = frame.name().map(|n| n.to_string());
        mk_traceback(
            py,
            func_name.as_deref().unwrap_or("<unknown function>"),
            frame
                .filename()
                .and_then(|f| f.to_str())
                .unwrap_or("<unknown>"),
            frame.lineno().map_or(0, |n| n as usize),
        )
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

/// Create a Python traceback object.
///
/// # Errors
/// Can only return an error if something is seriously wrong with the runtime.
pub fn mk_traceback<'py>(
    py: Python<'py>,
    name: &str,
    filename: &str,
    lineno: usize,
) -> PyResult<Bound<'py, PyTraceback>> {
    MK_TRACEBACK
        .call(py, (name, filename, lineno), None)?
        .into_bound(py)
        .extract()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use assertables::*;
    use pyo3::exceptions::PyRuntimeError;
    use regex::Regex;

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

        assert_is_match!(
            Regex::new(r#"File "[.]/src/lib.rs", line \d+, in pyo3_err_bridge::tests::anyhow"#)
                .unwrap(),
            out.as_str()
        );
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

        assert_is_match!(Regex::new(r#"File ".+pyo3-err-bridge/src/lib.rs", line \d+, in pyo3_err_bridge(\[.*\])?::tests::color_eyre"#).unwrap(), out.as_str());
        Ok(())
    }
}
