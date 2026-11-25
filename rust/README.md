# Rust workspace

The workspace has six crates: `nivra-core`, `nivra-cli` (binaries `nivra` and
`nivra-analysis`), `nivra-store`, `nivra-tools`, `nivra-research` and
`nivra-sandbox`. `rust-toolchain.toml` pins the toolchain and `Cargo.lock` pins
dependencies. The version comes from the root `Cargo.toml`.

See [architecture](../docs/ARCHITECTURE.md) for crate ownership and
[development](../docs/DEVELOPMENT.md) for the build and test gate.
