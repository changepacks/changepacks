# PROJECT KNOWLEDGE BASE

**Generated:** 2026-01-12T23:10:00+09:00
**Commit:** f0e85dc
**Branch:** main

## OVERVIEW

Rust-powered CLI for unified version management and changelog generation across Node.js, Python, Rust, and Dart monorepos. Inspired by changesets.

## STRUCTURE

```
changepacks/
├── crates/           # Rust workspace crates
│   ├── changepacks/  # Main binary entry point
│   ├── cli/          # Command handlers (check, update, publish, init, config)
│   ├── core/         # Traits: Package, Workspace, ProjectFinder
│   ├── utils/        # Git ops, version calc, dependency sorting
│   ├── node/         # Node.js (package.json) support
│   ├── python/       # Python (pyproject.toml) support
│   ├── rust/         # Rust (Cargo.toml) support
│   └── dart/         # Dart (pubspec.yaml) support
├── bridge/           # FFI bindings for package managers
│   ├── node/         # N-API bindings (@napi-rs)
│   └── python/       # PyO3 bindings (maturin)
├── examples/         # Test fixtures by language (node, python, dart)
└── .changepacks/     # Config and changepack logs
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add CLI command | `crates/cli/src/commands/` | Match existing command pattern |
| Add language support | `crates/{lang}/` | Implement `Package`, `Workspace`, `ProjectFinder` traits |
| Modify version logic | `crates/utils/src/` | `gen_update_map.rs`, `sort_by_dep.rs` |
| Core traits | `crates/core/src/` | `package.rs`, `workspace.rs`, `project.rs` |
| Git operations | `crates/utils/src/` | Uses `gix` crate |
| Node FFI | `bridge/node/` | N-API with `@napi-rs/cli` |
| Python FFI | `bridge/python/` | PyO3 with maturin |
| Config format | `.changepacks/config.json` | ignore, baseBranch, publish, updateOn |

## CONVENTIONS

### Rust
- Edition 2024, resolver 3
- Async-first with `tokio` (rt-multi-thread)
- `#[tokio::test]` for async tests, `#[test]` for sync
- Traits in `core`, implementations in language crates
- `anyhow` for error handling

### File Format Preservation
- JSON: `serde_json` with `preserve_order`
- TOML: `toml_edit` for non-destructive edits
- YAML: language-specific parsers maintain formatting

### Dependencies
- Workspace dependencies defined in root `Cargo.toml`
- Version refs: `changepacks-{crate}.workspace = true`

## ANTI-PATTERNS

- **Never** use `toml` crate for writing (destroys formatting) - use `toml_edit`
- **Never** hardcode publish commands - respect `.changepacks/config.json`
- **Never** skip topological sort when publishing - dependencies must publish first

## COMMANDS

```bash
# Development
cargo build              # Build all crates
cargo test               # Run all tests
cargo clippy             # Lint check

# Full build (Rust + Node + Python bridges)
bun run build            # cargo build --release && bun workspaces build && maturin build

# Lint all
bun run lint             # cargo clippy + cargo fmt --check + bun workspaces lint
```

## NOTES

- **No CI workflows** in repo - builds/tests run locally
- **Bridge packages** auto-update when core crates change (see `updateOn` in config)
- **Examples** are test fixtures, not production code
- **Typo**: `crates/core/src/proejct_finder.rs` (project misspelled)
