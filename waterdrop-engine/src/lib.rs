//! # waterdrop-engine
//!
//! Runtime logic for WaterDrop, embedded into both the CLI and the desktop UI.
//!
//! This crate provides:
//! - **Engine** with dual-mode operation: server (accept inbound connections)
//!   and client (initiate outbound connections)
//! - **Session state machine**: event-driven, cancellable protocol sessions
//!   over any transport (`Connection` trait)
//! - **Transport implementations**: TCP (yamux-multiplexed) and QUIC
//! - **Event bus**: emits events (offer, progress, done, errors) consumed by
//!   CLI loggers or UI subscribers

pub mod engine;
pub mod quic;
pub mod session;
pub mod tcp;
