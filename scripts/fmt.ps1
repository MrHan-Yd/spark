$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
