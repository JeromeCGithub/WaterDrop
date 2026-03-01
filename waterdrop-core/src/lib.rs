//! # waterdrop-core
//!
//! Shared building blocks for the WaterDrop file drop protocol.
//!
//! This crate provides the foundational types and utilities used by
//! [`waterdrop-engine`] and the binary crates (`waterdrop-cli`, `waterdrop-app`).
//!
//! ## Responsibilities
//!
//! - **Protocol primitives** — frame format (magic / version / type / length),
//!   message structs and enums for file transfer, and JSON payload
//!   encoding/decoding.
//!
//! - **TLS helpers** — self-signed certificate generation for transport security.
//!
//! - **Filesystem helpers** — filename sanitisation, temp-file write with atomic
//!   rename, and collision rename strategy (e.g. `file (1).ext`).

pub mod protocol;
pub mod tls;
pub mod transport;
