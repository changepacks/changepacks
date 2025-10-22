#[pyo3_async_runtimes::tokio::main]
async fn main() -> pyo3::prelude::PyResult<()> {
    cli::main(&std::env::args().skip(1).collect::<Vec<String>>())
        .await
        .map_err(|e| pyo3::prelude::PyErr::new::<pyo3::exceptions::PyException, _>(e.to_string()))
}
