use futures::stream::BoxStream;
use futures::TryStreamExt;
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::sync::Arc;

type ByteStream = BoxStream<'static, PyResult<bytes::Bytes>>;

/// Contents read from a store.
///
/// Iterate over it with ``async for`` to get the contents in chunks, or
/// await ``read()`` to get all of it at once.
#[pyclass]
pub(crate) struct AsyncReader(Arc<tokio::sync::Mutex<ByteStream>>);

impl AsyncReader {
    pub fn new(stream: ByteStream) -> Self {
        Self(Arc::new(tokio::sync::Mutex::new(stream)))
    }
}

#[pymethods]
impl AsyncReader {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyAny>> {
        let stream = self.0.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let chunk = stream
                .lock()
                .await
                .try_next()
                .await?
                .ok_or_else(|| PyStopAsyncIteration::new_err(()))?;
            Ok(Python::attach(|py| PyBytes::new(py, &chunk).unbind()))
        })
    }

    /// Read the remaining contents.
    fn read<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyAny>> {
        let stream = self.0.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut stream = stream.lock().await;
            let mut contents = Vec::new();
            while let Some(chunk) = stream.try_next().await? {
                contents.extend_from_slice(&chunk);
            }
            Ok(Python::attach(|py| PyBytes::new(py, &contents).unbind()))
        })
    }
}
