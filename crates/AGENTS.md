# CRATES WORKSPACE

Rust workspace crates implementing the changepacks CLI.

## WHERE TO LOOK

| Task | Crate | File Pattern |
|------|-------|--------------|
| Add CLI command | `cli` | `src/commands/{name}.rs`, update `mod.rs` |
| Add CLI option | `cli` | `src/options/{name}_options.rs` |
| Add language support | `{lang}` | `finder.rs`, `package.rs`, `workspace.rs`, `lib.rs` |
| Modify core traits | `core` | `package.rs`, `workspace.rs`, `proejct_finder.rs` |
| Add utility function | `utils` | New file in `src/`, export in `lib.rs` |
| Version calculation | `utils` | `next_version.rs`, `split_version.rs` |
| Dependency ordering | `utils` | `sort_by_dep.rs` |
| Git operations | `utils` | `find_current_git_repo.rs` |
| Config management | `utils` | `get_changepacks_config.rs`, `get_changepacks_dir.rs` |

## CONVENTIONS

### Language Crate Pattern

Each language crate (`node`, `python`, `rust`, `dart`) follows identical structure:

```
{lang}/src/
├── lib.rs         # Re-exports, language-specific helpers
├── finder.rs      # impl ProjectFinder - discovers projects
├── package.rs     # impl Package - single package handling
└── workspace.rs   # impl Workspace - monorepo root handling
```

All implement these `core` traits:
- `Package` - version updates, publish commands, change detection
- `Workspace` - same as Package but for workspace roots
- `ProjectFinder` - file discovery, project visiting

### CLI Command Pattern

Each command in `cli/src/commands/`:
- Define `{Command}Args` struct with clap derives
- Implement `handle_{command}` async function
- Export both in `mod.rs`

### Testing

- Inline tests with `#[cfg(test)]` module at file bottom
- Use `#[tokio::test]` for async, `#[test]` for sync
- `tempfile::TempDir` for filesystem tests
- Mock implementations for trait testing

### Utils Module Pattern

- One focused utility per file
- Export function via `pub use` in `lib.rs`
- Name file after primary function (e.g., `next_version.rs` exports `next_version`)

## ANTI-PATTERNS

- **Never** add workspace logic to `package.rs` or vice versa
- **Never** put language-specific code in `core` - traits only
- **Never** duplicate trait implementations - extract to `core`
- **Never** import language crates into each other
- **Never** use blocking I/O - all file ops via `tokio::fs`
