<p align="center">
  <img src="media/logo.svg" alt="changepacks logo" width="200"/>
</p>

[![Crates.io](https://img.shields.io/crates/v/changepacks)](https://crates.io/crates/changepacks)
[![PyPI](https://img.shields.io/pypi/v/changepacks)](https://pypi.org/project/changepacks)
[![npm bundle size](https://img.shields.io/bundlephobia/minzip/@changepacks/cli)](https://bundlephobia.com/package/@changepacks/cli)
[![npm version](https://img.shields.io/npm/v/@changepacks/cli)](https://www.npmjs.com/package/@changepacks/cli)
[![npm downloads](https://img.shields.io/npm/dm/@changepacks/cli)](https://www.npmjs.com/package/@changepacks/cli)
[![license](https://img.shields.io/github/license/changepacks/changepacks)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/changepacks/changepacks/CI.yml?branch=main)](https://github.com/changepacks/changepacks/actions)
[![codecov](https://img.shields.io/codecov/c/github/changepacks/changepacks?logo=codecov)](https://codecov.io/gh/changepacks/changepacks)
[![GitHub stars](https://img.shields.io/github/stars/changepacks/changepacks?style=social&label=Star)](https://github.com/changepacks/changepacks)
[![GitHub forks](https://img.shields.io/github/forks/changepacks/changepacks?style=social&label=Fork)](https://github.com/changepacks/changepacks/fork)
[![GitHub issues](https://img.shields.io/github/issues/changepacks/changepacks)](https://github.com/changepacks/changepacks/issues)
[![GitHub pull requests](https://img.shields.io/github/issues-pr/changepacks/changepacks)](https://github.com/changepacks/changepacks/pulls)
[![GitHub last commit](https://img.shields.io/github/last-commit/changepacks/changepacks)](https://github.com/changepacks/changepacks/commits/main)
[![Rust](https://img.shields.io/badge/Rust-1.72%2B-orange.svg)](https://rust-lang.org/)
[![Python](https://img.shields.io/badge/python-3.8+-blue.svg)](https://www.python.org/)
[![Node.js](https://img.shields.io/badge/Node.js-18%2B-green.svg)](https://nodejs.org/)
[![Bun](https://img.shields.io/badge/Bun-latest-000000.svg)](https://bun.sh)

# changepacks 📦

A unified version management and changelog tool for multi-language monorepos.

## Overview

**changepacks** is a Rust-powered CLI tool that brings consistent version management and changelog generation to polyglot projects. Inspired by [changesets](https://github.com/changesets/changesets), it extends beyond JavaScript to natively support Node.js, Python, Rust, and Dart ecosystems with a single, fast, and reliable tool.

### Why changepacks?

- 🌍 **True Multi-Language Support** - Manage versions across Node.js, Python, Rust, and Dart with unified workflow
- 🚀 **Rust Performance** - Fast, parallel operations with single binary distribution
- 🔗 **Smart Dependencies** - Automatic dependency resolution with topological sorting for publishing
- 💾 **Format Preservation** - Respects language conventions (JSON indentation, TOML formatting, YAML structure)
- 🌳 **Git-Native** - Uses git history for intelligent change detection
- 🎯 **Developer-Friendly** - Interactive CLI with colored output, tree views, and clear error messages

## Recent Improvements

- ✨ **Enhanced Readability** - Patch lists now display with newlines instead of commas for better visibility
- 📦 **Dependency Sorting** - Topological sort ensures packages publish in correct order
- 🎨 **Improved Output** - Tree view with colorized status and dependency visualization
- 🔧 **Format Preservation** - Maintains your file formatting (indentation, newlines) across all updates

## Features

- 🚀 **Multi-language Support** - Native support for Node.js, Python, Rust, and Dart with workspace detection
- 📝 **Changepack Logs** - Track version updates with timestamped logs and detailed notes
- 🔄 **Automated Updates** - Smart version bumping with workspace dependency updates
- 🔗 **Dependency Resolution** - Topological sorting ensures dependencies publish before dependents
- ⚡ **Fast CLI** - Async Rust implementation with parallel operations
- 🎯 **Project Detection** - Automatic discovery of packages and workspaces via git
- 📊 **Status & Visualization** - Tree view of dependencies with change markers
- 💾 **Format Preservation** - Maintains indentation, newlines, and file formatting
- 🧪 **Testing Support** - Dry-run mode for updates and publishing
- 🔧 **Configurable** - Custom publish commands, ignore patterns, and base branch

## Supported Languages & Package Managers

| Language | Package Manager | File | Status |
|----------|----------------|------|--------|
| **Node.js** | npm, pnpm, yarn | `package.json` | ✅ Supported |
| **Python** | pip, uv | `pyproject.toml` | ✅ Supported |
| **Rust** | Cargo | `Cargo.toml` | ✅ Supported |
| **Dart** | pub | `pubspec.yaml` | ✅ Supported |

## Installation

Choose your preferred package manager:

### Windows
```bash
winget install Changepacks.Changepacks
```

### Rust (Cargo)
```bash
cargo install changepacks
```

### Python
```bash
# pip
pip install changepacks

# uv (recommended)
uv add changepacks
uvx changepacks
```

### Node.js
```bash
# npm
npm install @changepacks/cli
npx @changepacks/cli

# pnpm
pnpm install @changepacks/cli
pnpm dlx @changepacks/cli

# yarn
yarn install @changepacks/cli

# bun
bun install @changepacks/cli
bunx @changepacks/cli
```

### Requirements

- Git repository (for project detection)
- Rust 1.91+ (for building from source)

### Build from Source

```bash
git clone https://github.com/changepacks/changepacks.git
cd changepacks
cargo build --release
```

The binary will be available at `target/release/changepacks` (or `target/release/changepacks.exe` on Windows).

## Usage

### Quick Start

1. **Initialize** changepacks in your repository:
```bash
changepacks init
```

2. **Create a changepack** when you make changes:
```bash
changepacks
```
This opens an interactive session to select changed projects and write release notes.

3. **Update versions** from changepack logs:
```bash
changepacks update
```

4. **Publish** packages in dependency order:
```bash
changepacks publish
```

### Typical Workflow

```bash
# 1. Check which projects have changed
changepacks check --tree

# 2. Create a changepack log for your changes
changepacks
# → Select projects (Major/Minor/Patch)
# → Write changelog notes

# 3. Preview version updates
changepacks update --dry-run

# 4. Apply version updates
changepacks update

# 5. Test publishing
changepacks publish --dry-run

# 6. Publish to registries
changepacks publish
```

### Check Project Status

View all projects with change detection:

```bash
changepacks check              # List all projects
changepacks check --tree       # Show dependency tree
changepacks check --filter workspace  # Only workspaces
changepacks check --filter package    # Only packages
changepacks check --remote     # Compare with remote branch
```

### Update Versions

Apply version bumps from changepack logs:

```bash
changepacks update              # Interactive confirmation
changepacks update --dry-run    # Preview without applying
changepacks update --yes        # Skip confirmation
```

### Publish Packages

Publish packages to their respective registries:

```bash
changepacks publish
```

Options:

```bash
changepacks publish --dry-run           # Preview what would be published without actually publishing
changepacks publish --yes               # Skip confirmation prompts
changepacks publish --format json       # Output results in JSON format
changepacks publish --remote            # Use remote branch for change detection
```

The publish command will:
1. Discover all projects in your workspace
2. Show which projects will be published
3. Execute the publish command for each project (using language-specific defaults or custom commands from config)

Default publish commands by language:
- **Node.js**: `npm publish`
- **Python**: `uv publish`
- **Rust**: `cargo publish`
- **Dart**: `dart pub publish`

### Check Config

View the loaded changepacks config (from `.changepacks/config.json`):

```bash
changepacks config
```

This prints the merged and defaulted configuration, for example:

```json
{
  "ignore": [
    "**/*",
    "!crates/changepacks/Cargo.toml",
    "!bridge/node/package.json",
    "!bridge/python/pyproject.toml"
  ],
  "baseBranch": "main",
  "latestPackage": "crates/changepacks/Cargo.toml",
  "publish": {
    "node": "npm publish",
    "python": "uv publish",
    "rust": "cargo publish",
    "dart": "dart pub publish",
    "bridge/node/package.json": "npm publish --access public"
  }
}
```

You can edit `.changepacks/config.json` to customize:
- Files/projects to ignore (`ignore`) using glob patterns (default: empty).
- The base branch to compare against for changes (`baseBranch`, default: `"main"`).
- The default main package for versioning (`latestPackage`, optional).
- Custom publish commands (`publish`):
  - Set language-specific commands using language keys: `"node"`, `"python"`, `"rust"`, `"dart"`.
  - Set project-specific commands using relative paths (e.g., `"bridge/node/package.json"`).
  - If not specified, default commands are used (see Publish Packages section).

If the config file is missing or empty, sensible defaults are used.

### Default Command

Running `changepacks` without arguments starts an interactive session to select projects and create a changepack log.

## Project Structure

```
changepacks/
├── crates/
│   ├── cli/          # CLI interface and commands
│   ├── core/         # Core types and traits
│   ├── node/         # Node.js project support
│   ├── python/       # Python project support
│   ├── rust/         # Rust project support
│   ├── dart/         # Dart project support
│   └── utils/        # Utility functions
├── examples/         # Example projects for testing
├── Cargo.toml        # Workspace configuration
└── README.md
```

## How It Works

1. **Project Detection**: Walks git tree to discover `package.json`, `Cargo.toml`, `pyproject.toml`, `pubspec.yaml` files
2. **Change Tracking**: Uses git diff to detect changed files, marking projects with modifications
3. **Changepack Logs**: Stores version bump intentions in `.changepacks/changepack_log_*.json` with notes and timestamps
4. **Version Updates**: Reads changepack logs, calculates new versions (semver), updates files while preserving formatting
5. **Dependency Resolution**: Topologically sorts projects by dependencies for correct publish order
6. **Publishing**: Executes language-specific or custom publish commands in dependency order

### Changepack Log Format

```json
{
  "changes": {
    "packages/foo/package.json": "Minor",
    "crates/bar/Cargo.toml": "Patch"
  },
  "note": "Add new feature X and fix bug Y",
  "date": "2025-12-19T10:27:00.000Z"
}
```

## Development

### Build Workspace

```bash
cargo build
```

### Run Tests

```bash
cargo test
```

### Lint Check

```bash
cargo clippy
```

### Run Examples

Test with example projects:

```bash
cd examples/node/common
changepacks check
```

## Architecture

The project follows a trait-based, modular architecture:

- **Core** (`crates/core`) - Defines common traits (`Package`, `Workspace`, `ProjectFinder`) and types
- **Language Crates** (`crates/{node,python,rust,dart}`) - Implement language-specific project detection and version management
- **CLI** (`crates/cli`) - Command-line interface with clap, colored output, and interactive prompts
- **Utils** (`crates/utils`) - Shared utilities: git operations, version calculation, dependency sorting, config management
- **Bridges** (`bridge/{node,python}`) - N-API and PyO3 bindings for package manager distribution

### Key Design Patterns

- **Async-First**: All I/O operations use tokio for parallel execution
- **Format Preservation**: Language-specific parsers (toml_edit, yamlpatch, serde_json) maintain file formatting
- **Git-Native**: Uses git2 library for change detection and repository operations
- **Topological Sorting**: Kahn's algorithm ensures correct publish order based on dependencies

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## Sponsors

We're grateful to our sponsors for supporting changepacks development! If you're interested in sponsoring this project, please get in touch.

<div align="center">

<!-- Add your sponsors here -->
<!-- 
<a href="https://example.com">
  <img src="https://example.com/logo.png" width="150" alt="Sponsor Name" />
  <p><strong>Sponsor Name</strong></p>
</a>

<a href="https://example.com">
  <img src="https://example.com/logo2.png" width="150" alt="Another Sponsor" />
  <p><strong>Another Sponsor</strong></p>
</a>
-->

</div>

## Used By

The following open-source projects and companies are using changepacks:

<div align="center">

<a href="https://devup-ui.com">
  <img src="https://raw.githubusercontent.com/dev-five-git/devup-ui/main/media/logo.svg" width="150" alt="Devup UI" />
  <p><strong>Devup UI</strong></p>
</a>
<a href="https://braillify.kr">
  <img src="https://raw.githubusercontent.com/dev-five-git/braillify/main/media/logo.svg" width="150" alt="Braillify" />
  <p><strong>Braillify</strong></p>
</a>
<a href="https://devfive.kr">
  <img src="https://avatars.githubusercontent.com/u/85065616?s=200&v=4" width="150" alt="Braillify" />
  <p><strong>Devfive</strong></p>
</a>

<!-- Add your projects here -->
<!-- 
<a href="https://github.com/user/project">
  <img src="https://example.com/logo.png" width="150" alt="Project Name" />
  <p><strong>Project Name</strong></p>
</a>

<a href="https://example.com">
  <img src="https://example.com/company-logo.png" width="150" alt="Company Name" />
  <p><strong>Company Name</strong></p>
</a>
-->

</div>

If you're using changepacks in your project, we'd love to feature you here! Please open a [Pull Request](https://github.com/changepacks/changepacks/pulls) to add your project or company.

## License

This project is distributed under the MIT License. See the [LICENSE](LICENSE) file for more details.

## Roadmap

- [x] Node.js package management support
- [x] Python package management support
- [x] Rust package management support
- [x] Dart package management support
- [x] CI/CD integration support (JSON output, dry-run mode)
- [x] Dependency-aware publishing with topological sorting
- [x] Format preservation across all languages
- [x] Interactive CLI with tree view and colored output
- [x] Cross-platform distribution (Windows, macOS, Linux)
- [x] N-API and PyO3 bindings for npm/PyPI
- [ ] Plugin system for additional languages
- [ ] CHANGELOG.md generation from changepack logs
- [ ] GitHub Actions integration
- [ ] Pre-release version support

## Support

If you encounter any issues or have feature requests, please let us know on the [Issues](https://github.com/changepacks/changepacks/issues) page.

## Inspirations

- [changesets](https://github.com/changesets/changesets) - Version management for JavaScript projects
