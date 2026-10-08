# <img src=".github/logo.png" width="48" height="48" align="top"> Juiceutils

Shared Rust library for [juicehost](https://github.com/juiceboxdev/juicehost). It provides stuff like file-type validation, IP-ban helpers, trusted-proxy address resolution, security headers, shutdown handling, and an optional HTTP/3 server bridge.

juiceutils is normally checked out as a juicehost Git submodule. Clone juicehost recursively:

```sh
git clone --recurse-submodules https://github.com/juiceboxdev/juicehost.git
```

## Features

The default `quic` feature enables the QUIC/HTTP/3 server, certificate helpers, and related dependencies. Disable it when only the validation, ban, proxy, and HTTP helpers are needed:

```toml
juiceutils = { path = "juiceutils", default-features = false }
```

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --all-targets --no-default-features
```

## License

LGPL-3.0-or-later. See [LICENSE](LICENSE).

Report vulnerabilities through the private reporting process in [SECURITY.md](SECURITY.md).
