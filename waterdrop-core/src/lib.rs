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
//!   message structs and enums for pairing and file transfer, and JSON payload
//!   encoding/decoding.
//!
//! - **Identity & authentication** — device Ed25519 keypair generation,
//!   `device_id` derivation (public-key fingerprint), and signature helpers.
//!
//! - **Persistence models** — paired-device records and configuration model.
//!
//! - **Filesystem helpers** — filename sanitisation, temp-file write with atomic
//!   rename, and collision rename strategy (e.g. `file (1).ext`).
