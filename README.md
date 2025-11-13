<p align="center">
  <img src="media/logo.svg" alt="changepacks logo" width="200"/>
</p>

# changepacks 📦

A unified version management and changelog tool for multi-language projects.

## Overview

changepacks is a CLI tool that helps you efficiently manage versioning and changelogs across different programming languages and package managers. It provides a unified interface for managing versions in Node.js, Python, Rust, and Dart projects.

## Features

- 🚀 **Multi-language Support**: Native support for Node.js, Python, Rust, and Dart
- 📝 **Unified Version Management**: Consistent versioning across different package managers
- 🔄 **Automated Updates**: Smart version bumping based on project changes
- ⚡ **CLI Interface**: Simple and intuitive command-line interface
- 🎯 **Project Detection**: Automatic detection of projects in your workspace
- 📊 **Status Tracking**: Track which projects need version updates

## Supported Languages & Package Managers

| Language | Package Manager | File | Status |
|----------|----------------|------|--------|
| **Node.js** | npm, pnpm, yarn | `package.json` | ✅ Supported |
| **Python** | pip, uv | `pyproject.toml` | ✅ Supported |
| **Rust** | Cargo | `Cargo.toml` | ✅ Supported |
| **Dart** | pub | `pubspec.yaml` | ✅ Supported |

## Installation

```bash
winget install Changepacks.Changepacks

cargo install changepacks

pip install changepacks
uv add changepacks
# run
uvx changepacks

npm install @changepacks/cli
bun install @changepacks/cli
pnpm install @changepacks/cli
yarn install @changepacks/cli
# run
npx @changepacks/cli
bunx @changepacks/cli
pnpm dlx @changepacks/cli
```

### Requirements

- Rust 1.91+ (for development)
- Cargo
- Git repository (for project detection)

### Build from Source

```bash
git clone https://github.com/changepacks/changepacks.git
cd changepacks
cargo build --release
```

The binary will be available at `target/release/changepacks` (or `target/release/changepacks.exe` on Windows).

## Usage

### Initialize Project

Initialize changepacks in your repository:

```bash
changepacks init
```

This creates a `.changepacks/` directory with configuration files.

### Check Project Status

Discover and display all projects in your workspace:

```bash
changepacks check
```

Filter by project type:

```bash
changepacks check --filter workspace  # Show only workspaces
changepacks check --filter package    # Show only packages
```

### Update Versions

Update project versions based on changes:

```bash
changepacks update
```

Options:

```bash
changepacks update --dry-run    # Preview changes without applying
changepacks update --yes        # Skip confirmation prompts
```

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
  "latestPackage": "crates/changepacks/Cargo.toml"
}
```

You can edit `.changepacks/config.json` to customize:
- Files/projects to ignore (`ignore`) using glob patterns (default: empty).
- The base branch to compare against for changes (`baseBranch`, default: `"main"`).
- The default main package for versioning (`latestPackage`, optional).

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

1. **Project Detection**: Scans your repository for supported project files
2. **Change Tracking**: Monitors file changes to determine which projects need updates
3. **Version Management**: Provides unified version bumping across different package managers
4. **Update Coordination**: Ensures consistent versioning across related projects

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

The project is built with a modular architecture:

- **Core**: Defines common traits and types for workspaces and packages
- **Language Crates**: Implement language-specific project detection and management
- **CLI**: Provides the user interface and command orchestration
- **Utils**: Shared utilities for path handling, version calculation, and more

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
- [X] CI/CD integration support
- [ ] Plugin system for additional languages

## Support

If you encounter any issues or have feature requests, please let us know on the [Issues](https://github.com/changepacks/changepacks/issues) page.

## Inspirations

- [changesets](https://github.com/changesets/changesets) - Version management for JavaScript projects
