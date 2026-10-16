# Recipes are plain cargo calls, so PowerShell runs them as well as sh.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# Scan PATH, or the current folder: `just run ~/Photos`.
run *path:
    cargo run --release --bin onlyfile -- {{path}}

# Everything CI checks, in the same order. Run it before pushing.
gate: fmt-check lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace
