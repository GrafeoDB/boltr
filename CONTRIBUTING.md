# Contributing to BoltR

Thank you for your interest in contributing to BoltR! This document provides guidelines for contributing to the project.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/boltr.git`
3. Create a branch: `git checkout -b your-feature`
4. Make your changes
5. Submit a pull request

## Development Setup

### Prerequisites

- Rust 1.85.0+ (edition 2024)
- `cargo` (comes with Rust)

### Building

```bash
cargo build                               # Debug build
cargo build --release                     # Release build
cargo build --all-features                # Build with all features (TLS, client)
```

### Running Tests

```bash
cargo test                                # Full test suite
cargo test --all-features                 # With all features
cargo test test_name -- --nocapture       # Single test with output
```

### Linting and Formatting

All code must pass clippy and rustfmt before merging:

```bash
cargo fmt --all                           # Format code
cargo fmt --all -- --check                # Check formatting
cargo clippy --all-targets --all-features # Run clippy
```

## Code Style

See [.claude/CODE_STYLE.md](.claude/CODE_STYLE.md) for the full style guide. Key points:

- Use `thiserror` for error types, chain with `#[source]`
- No abbreviations in public APIs (use `Connection` not `Conn`)
- Borrow by default, zero-copy where possible
- All public items must be documented
- `#![forbid(unsafe_code)]` - no unsafe code
- Round-trip tests for all PackStream types

## What to Contribute

### Good First Issues

- Adding missing PackStream round-trip tests
- Improving error messages
- Documentation improvements
- Adding examples

### Areas of Interest

- Bolt protocol version support expansion
- Performance improvements (with benchmarks)
- Additional authentication mechanisms
- Client library enhancements

## Pull Request Process

1. Ensure your code compiles with no warnings (`cargo clippy`)
2. Ensure all tests pass (`cargo test --all-features`)
3. Ensure code is formatted (`cargo fmt --all -- --check`)
4. Write tests for new functionality
5. Update documentation if you change public APIs
6. Keep commits focused and well-described

## Commit Messages

- Use the imperative mood ("Add feature" not "Added feature")
- Keep the first line under 72 characters
- Reference issues where applicable

## Architecture

See [.claude/ARCHITECTURE.md](.claude/ARCHITECTURE.md) for an overview of the codebase structure, protocol layers, and design decisions.

## License

By contributing, you agree that your contributions will be licensed under the same dual license as the project: MIT OR Apache-2.0.
