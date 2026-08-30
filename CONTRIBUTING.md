# Contributing

Thanks for considering a contribution to `logtail`.

## Development setup

You need a Rust toolchain at or above the MSRV declared in `Cargo.toml`
(`rust-version`, currently 1.85). Install via [rustup](https://rustup.rs/).

```sh
cargo build
cargo test --all-targets
```

## Before opening a pull request

Run the same gates CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

All three must pass cleanly. `unsafe_code` is forbidden crate-wide; new code
should not need it. `clippy::unwrap_used` and `clippy::expect_used` are
denied outside `#[cfg(test)]` modules - handle errors explicitly in library
and binary code.

## Style

- Keep dependencies minimal; the MSRV is kept low deliberately, and every
  new dependency is a potential MSRV bump.
- Favor streaming/bounded-memory implementations over ones that buffer a
  whole file, consistent with the rest of the codebase.
- Add tests alongside new functionality: unit tests in the module (under
  `#[cfg(test)]`), integration/end-to-end behavior in `tests/`.
- Update `CHANGELOG.md` under an `[Unreleased]` heading for user-visible
  changes.

## Reporting bugs

Open an issue with: the query or command you ran, the input (or a minimal
reproduction), what you expected, and what happened instead.
