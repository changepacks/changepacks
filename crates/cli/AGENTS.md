# CLI Crate

Command-line interface for changepacks using clap with async command handlers.

## STRUCTURE

```
src/
├── lib.rs              # Cli struct, Commands enum, main() dispatch
├── commands/
│   ├── mod.rs          # Re-exports {Command}Args + handle_{command}
│   ├── changepacks.rs  # Default interactive mode (no subcommand)
│   ├── check.rs        # Project status and dependency tree
│   ├── config.rs       # Display loaded config
│   ├── init.rs         # Initialize .changepacks directory
│   ├── publish.rs      # Publish packages in dependency order
│   └── update.rs       # Apply version updates from logs
├── options/
│   ├── mod.rs          # FilterOptions, FormatOptions
│   └── language_options.rs  # CliLanguage enum for --language flag
├── finders.rs          # get_finders() returns all ProjectFinder impls
└── prompter.rs         # Prompter trait + InquirePrompter/MockPrompter
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add new subcommand | `commands/{name}.rs` | Create file, add to mod.rs + lib.rs |
| Add shared CLI flag | `options/` | Create ValueEnum, import in commands |
| Modify interactive prompts | `prompter.rs` | Implement Prompter trait |
| Change command dispatch | `lib.rs` | Commands enum + main() match |
| Add language filter | `options/language_options.rs` | CliLanguage enum |

## ADDING A COMMAND

1. **Create command file** `src/commands/{name}.rs`:

```rust
use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
#[command(about = "Short description")]
pub struct NameArgs {
    #[arg(short, long)]
    pub dry_run: bool,
}

pub async fn handle_name(args: &NameArgs) -> Result<()> {
    // Implementation
    Ok(())
}
```

2. **Export in** `src/commands/mod.rs`:

```rust
mod name;
pub use name::NameArgs;
pub use name::handle_name;
```

3. **Register in** `src/lib.rs`:

```rust
// In Commands enum:
Name(NameArgs),

// In main() match:
Commands::Name(args) => handle_name(&args).await?,
```

## PATTERNS

- **Testable prompts**: Commands accept `&dyn Prompter` for dependency injection
- **Output formats**: Use `FormatOptions::Stdout` vs `FormatOptions::Json`
- **Dry-run**: Add `#[arg(short, long)]` flag, skip mutations when true
- **Tests**: Parse args via wrapper struct with `#[command(flatten)]`
