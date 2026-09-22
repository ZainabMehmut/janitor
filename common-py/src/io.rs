use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::io::Read;

#[pyclass]
pub(crate) struct Readable(Box<dyn std::io::Read + Send + Sync>);

impl Readable {
    pub fn new(read: Box<dyn std::io::Read + Send + Sync>) -> Self {
        Self(read)
    }
}

#[pymethods]
impl Readable {
    #[pyo3(signature = (size=None))]
    fn read(&mut self, py: Python, size: Option<usize>) -> PyResult<Py<PyAny>> {
        match size {
            // Python's file-object convention: read() / read(None) means
            // "read until EOF", not "read one 4096-byte buffer" - the
            // previous default-to-4096-and-read-once behavior silently
            // truncated anything larger than one buffer (e.g. a real .deb
            // file), since std::io::Read::read() is allowed to return
            // fewer bytes than the buffer without that meaning EOF.
            None => {
                let mut buf = Vec::new();
                self.0.read_to_end(&mut buf).map_err(PyRuntimeError::new_err)?;
                Ok(PyBytes::new(py, &buf).into())
            }
            Some(n) => {
                let mut buf = vec![0; n];
                let read_n = self.0.read(&mut buf).map_err(PyRuntimeError::new_err)?;
                buf.truncate(read_n);
                Ok(PyBytes::new(py, &buf).into())
            }
        }
    }
}
