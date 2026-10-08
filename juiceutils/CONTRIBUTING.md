# Contributing

Use a recent stable Rust toolchain. Before submitting a change, run the formatting, Clippy, and feature-matrix commands documented in [README.md](README.md).

Generate the Rust API reference with:

```sh
cargo doc --no-deps --all-features
```

The generated documentation is available at `target/doc/juiceutils/index.html`.

Keep changes focused and add tests for behavioral changes. Do not commit environment files, certificates, private keys, or generated build artifacts.

By submitting a contribution, you agree that it may be distributed under the repository's LGPL-3.0-or-later license.

## Refrain from using AI

The use of LLM's and AI tools are allowed, however they should not be the source of truth for code generation. Use human judgment and existing code as a reference.
