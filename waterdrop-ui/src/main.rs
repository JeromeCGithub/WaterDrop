mod app;
mod components;
mod engine;
mod models;

use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("waterdrop_ui=info,waterdrop_engine=info,warn")),
        )
        .init();

    dioxus::launch(app::App);
}
