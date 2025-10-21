use pyo3::prelude::*;

#[pyo3_async_runtimes::tokio::main]
async fn main() -> PyResult<()> {
    cli::main(&std::env::args().skip(1).collect::<Vec<String>>())
        .await
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyException, _>(e.to_string()))
}
