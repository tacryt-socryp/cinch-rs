# Agents

## Pre-commit checks

Before committing, always run the following commands and fix any issues:

```sh
cargo fmt -- --check
cargo clippy -- -D warnings
```

If `cargo fmt -- --check` reports unformatted code, run `cargo fmt` to fix it.
If `cargo clippy -- -D warnings` reports warnings, fix the code so clippy passes.
