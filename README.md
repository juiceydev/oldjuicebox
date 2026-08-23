# <img src=".github/logo.png" width="48" height="48" align="top"> Juicebox²
### The blazingly fast self-hosted [catbox](https://litterbox.catbox.moe/) clone.

The current repository is the orchestrator that spawns the Juicebox² services. It Runs `juiceback`, `juicehost`, and `juicefront` as child processes manages their lifecycle.

Juicebox² is currently hosted at **[box.juicey.dev](https://box.juicey.dev)** as I'm writing this right now.

## Architecture
We have multiple services that makes juicebox² work.
| Service | Role | Port |
|---------|------|------|
| [juiceback](https://github.com/juiceboxdev/juiceback) | Manages the  API backend | 6401 |
| [juicehost](https://github.com/juiceboxdev/juicehost) | Storage server | 6402 |
| [juicefront](https://github.com/juiceboxdev/juicefront) | Web UI wrapper | 6400 |

> We also have juiceutils but it is simply an utility crate to avoid duplicate code, doesn't host anything.

## Quick Start

```bash
git clone --recursive https://github.com/juiceboxdev/juicebox.git
cd juicebox
cargo build
cd juicefront/ui && bun install && cd ../.. # npm install is also supported
# set environment variables per service OR here on the project's root.
cargo run
```

For a release run, build the UI before launching the services:

```bash
cd juicefront/ui
bun install && bun run build # or use the npm equivalents
cd ../..
cargo build --release
# set environment variables per service OR here on the project's root.
cargo run --release
```

Or run services individually:
```bash
cargo run --package juiceback --release
cargo run --package juicehost --release
cargo run --package juicefront --release
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

juicebox-plus and juicepipe are also LGPL-3.0-or-later.

## Contact

Open an issue on _GitHub_  or contact JuiceyDev at box@juicey.dev
