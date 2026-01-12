# BRIDGE

FFI bindings enabling npm (`@changepacks/cli`) and PyPI (`changepacks`) distribution.

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Node N-API bindings | `node/src/lib.rs` | Wraps `changepacks_cli::main()` as async NAPI function |
| Node entry point | `node/main.js` | Shebang CLI that calls exported `main()` |
| Python maturin binary | `python/src/main.rs` | Standalone tokio binary calling CLI |
| Python entry point | `python/changepacks/__main__.py` | Finds and exec's compiled binary |
| Cross-compile config | `{node,python}/.cargo/config.toml` | Linker and rustflags per target |
| Benchmarks | `node/benchmark/bench.ts` | tinybench comparing native vs JS |

## CONVENTIONS

### Node (N-API)
- **Crate type**: `cdylib` for dynamic library
- Uses `napi` v3 with `tokio_rt` feature for async support
- `napi-derive` for `#[napi]` proc macros
- Binary name defined in `package.json` under `napi.binaryName`
- Targets: `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`

### Python (PyO3/Maturin)
- **Bindings**: `bin` (not pyo3) - produces standalone executable
- `tokio` with `rt-multi-thread` and `macros` features
- Python stub (`__main__.py`) locates binary via `sysconfig` paths
- Requires Python >= 3.9

### Shared
- Both depend on `changepacks-cli.workspace = true`
- Neither publishes to crates.io (`publish = false`)
- Auto-update via `updateOn` in `.changepacks/config.json`

## BUILD COMMANDS

```bash
# Node
cd bridge/node
napi build --platform --release    # Build native module
napi prepublish -t npm             # Prepare for npm publish

# Python
cd bridge/python
maturin build --release            # Build wheel with binary

# Linting (Node only)
oxlint .                           # Lint JS/TS files
prettier . -w                      # Format
taplo format                       # Format TOML

# Benchmarks
bun run bench                      # node/benchmark/bench.ts
```
