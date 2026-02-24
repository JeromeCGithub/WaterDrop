# WaterDrop

> ⚠️ **This project is in early implementation and is not functional yet.** The foundational infrastructure (transport, engine, logging) is in place, but the protocol, authentication, pairing, and file transfer are not implemented. You cannot transfer files with WaterDrop today.

A LAN / Tailscale file drop tool (AirDrop-like) for **Linux**, **Android**, and **Windows**.

Send files quickly to a trusted device on your local network or over Tailscale — no cloud, no accounts, no fuss.

## Planned Features

- **Fast local transfers** over TCP on LAN or Tailscale
- **Device pairing** as the security model (code-based or NAS password)
- **Headless / NAS-friendly** — runs as a Docker container or background process
- **Desktop UI** planned via [Dioxus](https://dioxuslabs.com/)
- **Receiver-controlled** — accept/deny transfers interactively, or auto-accept in headless mode
- **Drop folder** — all accepted files land in a single configured directory

## Project Status

**Stage: scaffolding / infrastructure only — not functional.**

The project has a working async engine that can accept TCP connections and dispatch them to a session handler, but no protocol logic exists yet. Connecting to the server (e.g. via telnet) will result in an immediate disconnect because the session handler is a placeholder.

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

### Test

```sh
cargo test --workspace
```

## Documentation

| Document | Description |
|----------|-------------|
| [`doc/project.md`](doc/project.md) | Project goals, pairing model, transport decisions |
| [`doc/architecture.md`](doc/architecture.md) | Workspace structure and crate responsibilities |
| [`doc/protocol_v1.md`](doc/protocol_v1.md) | Wire protocol v1 specification (frames, messages, auth, transfers) |

## License

[MIT](LICENSE) — Copyright (c) 2026 Jérôme C.
