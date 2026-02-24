//! # waterdrop-engine
//!
//! Runtime logic for WaterDrop, embedded into both the CLI and the desktop UI.
//!
//! This crate provides:
//! - **TCP listener** and per-connection session task management
//! - **Authentication**: HELLO signature verification for paired devices,
//!   pairing mode gating with TTL
//! - **Pairing subsystem**: code pairing (interactive) and NAS password pairing
//!   (headless), with persistence of newly paired devices
//! - **Transfer manager**: incoming offer handling (accept/deny), byte stream
//!   reception, file finalization, and concurrent transfer support (one per connection)
//! - **Event bus**: emits events (offer, progress, done, errors) consumed by
//!   CLI loggers or UI subscribers

pub mod engine;
pub mod session;
pub mod tcp;
