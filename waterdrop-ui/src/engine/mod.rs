//! Engine integration layer.
//!
//! This module is the single bridge between the Dioxus UI and
//! `waterdrop-engine`.  It provides:
//!
//! - **`storage`** — persisting / loading saved devices to a JSON file.
//! - **`bridge`**  — starting the engine, sending files, and forwarding
//!   engine events into Dioxus signals so the UI can react.
//! - **`config`**  — application configuration (listen address / port)
//!   resolved from environment variables, a config file, or defaults.

pub mod bridge;
pub mod config;
pub mod storage;
