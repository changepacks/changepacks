# Changepack 📦

A version management and changelog tool with support for multiple programming languages.

## Overview

Changepack is a CLI tool that helps you efficiently manage versioning and changelogs in multi-language projects. It provides unified support for various package management systems including Node.js, Python, Rust, and more.

## Features

- 🚀 **Multi-language Support**: Support for Node.js, Python, Rust, and other languages
- 📝 **Changelog Management**: Automated changelog generation and management
- 🔄 **Version Management**: Unified version management system
- ⚡ **CLI Interface**: Simple command-line interface

## Installation

### Requirements

- Rust 1.90+ (for development)
- Cargo

### Build

```bash
git clone https://github.com/your-username/changepack.git
cd changepack
cargo build --release
```

## Usage

### Initialize Project

```bash
changepack init
```

### Check Project Status

```bash
changepack check
```

## Project Structure

```
changepack/
├── crates/
│   ├── cli/          # CLI interface
│   ├── core/         # Core logic
│   ├── node/         # Node.js support
│   ├── python/       # Python support
│   └── rust/         # Rust support
├── Cargo.toml        # Workspace configuration
└── README.md
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

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

This project is distributed under the MIT License. See the [LICENSE](LICENSE) file for more details.

## Roadmap

- [ ] Node.js package management support
- [ ] Python package management support  
- [ ] Rust package management support
- [ ] Automated changelog generation
- [ ] CI/CD integration support

## Support

If you encounter any issues or have feature requests, please let us know on the [Issues](https://github.com/your-username/changepack/issues) page.


## Inspirations
- [changesets](https://github.com/changesets/changesets)
