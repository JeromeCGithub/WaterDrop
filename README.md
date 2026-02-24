# WaterDrop

A LAN / Tailscale file drop tool (AirDrop-like) for **Linux**, **Android**, and **Windows**.

Send files quickly to a trusted device on your local network or over Tailscale — no cloud, no accounts, no fuss.

## Features

- **Fast local transfers** over TCP on LAN or Tailscale
- **Device pairing** as the security model (code-based or NAS password)
- **Headless / NAS-friendly** — runs as a Docker container or background process
- **Desktop UI** planned via [Dioxus](https://dioxuslabs.com/)
- **Receiver-controlled** — accept/deny transfers interactively, or auto-accept in headless mode
- **Drop folder** — all accepted files land in a single configured directory

## Project Status

> **Early development** — the transport layer, engine orchestration, and structured logging are in place. The protocol (authentication, pairing, file transfer) is designed but not yet implemented.

### What's implemented

- Cargo workspace with `waterdrop-core`, `waterdrop-engine`, and `waterdrop-cli`
- Transport abstraction traits (`Connection`, `Listener`, `ListenerFactory`) in `waterdrop-core`
- Concrete TCP transport (`TcpConnection`, `TcpListener`, `TcpListenerFactory`) in `waterdrop-engine`
- Async engine with command / event model (`EngineCmd` / `EngineEvent`) using `tokio::select!`
- `SessionHandler` trait for pluggable connection handling
- Structured logging with [`tracing`](https://docs.rs/tracing) and `RUST_LOG` support
- Workspace-wide Clippy enforcement (`clippy::all` + `clippy::pedantic` at deny level)
- Tests for engine orchestration and TCP transport (9 tests)

### What's next

- [ ] Protocol v1 implementation (frame parsing, message types)
- [ ] Ed25519 identity and authentication handshake
- [ ] Device pairing (code + NAS password)
- [ ] File transfer (offer → accept/deny → stream → finalize)
- [ ] Desktop UI with Dioxus (`waterdrop-app`)
- [ ] Android client
- [ ] CI pipeline (GitHub Actions)

## Architecture

```
waterdrop-cli ──┐
                ├──▶ waterdrop-engine ──▶ waterdrop-core
waterdrop-app ──┘
```

| Crate | Role |
|-------|------|
| **waterdrop-core** | Shared types — transport traits, protocol primitives, identity helpers, filesystem utilities |
| **waterdrop-engine** | Runtime logic — TCP transport, engine event loop, session handling, auth, pairing, transfers |
| **waterdrop-cli** | Headless binary — starts the engine, logs events, handles Ctrl+C graceful shutdown |
| **waterdrop-app** | *(planned)* Desktop UI binary with Dioxus |

The engine is **generic over transport and session handler**, making it fully testable with mocks.

See [`doc/architecture.md`](doc/architecture.md) for the full design and [`doc/protocol_v1.md`](doc/protocol_v1.md) for the wire protocol specification.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (edition 2024, resolver v3)

### Build

```sh
cargo build --workspace
```

### Run

```sh
cargo run --bin waterdrop-cli
```

The CLI starts the engine, binds a TCP listener on `127.0.0.1:9000`, and waits for connections. Press **Ctrl+C** to shut down gracefully.

### Logging

WaterDrop uses [`tracing`](https://docs.rs/tracing) with an `EnvFilter`. Control verbosity via `RUST_LOG`:

```sh
# Default (info)
cargo run --bin waterdrop-cli

# Debug — includes engine loop internals
RUST_LOG=debug cargo run --bin waterdrop-cli

# Trace — maximum verbosity
RUST_LOG=trace cargo run --bin waterdrop-cli

# Filter by crate
RUST_LOG=waterdrop_engine=debug,waterdrop_cli=info cargo run --bin waterdrop-cli
```

### Test

```sh
cargo test --workspace
```

### Lint

```sh
cargo clippy --workspace
```

The workspace enforces `clippy::all` and `clippy::pedantic` at the `deny` level, and `unsafe_code` is forbidden. See the root [`Cargo.toml`](Cargo.toml) for the full lint configuration.

## Quick Test with Telnet

Start the CLI in one terminal:

```sh
RUST_LOG=debug cargo run --bin waterdrop-cli
```

Connect from another:

```sh
telnet 127.0.0.1 9000
```

You should see the connection accepted in the engine logs. The connection closes immediately because the current session handler is a placeholder that only logs the peer address — the actual protocol is not yet implemented.

## Documentation

| Document | Description |
|----------|-------------|
| [`doc/project.md`](doc/project.md) | Project goals, pairing model, transport decisions |
| [`doc/architecture.md`](doc/architecture.md) | Workspace structure and crate responsibilities |
| [`doc/protocol_v1.md`](doc/protocol_v1.md) | Wire protocol v1 specification (frames, messages, auth, transfers) |

## License

[MIT](LICENSE) — Copyright (c) 2026 Jérôme C.