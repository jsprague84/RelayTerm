//! Tracing setup for the backend binary.
//!
//! Centralised here so every entry point initialises logging the same way.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Initialise the global tracing subscriber.
///
/// Reads `RUST_LOG` for filter directives, defaulting to `info` for app crates
/// and `warn` for everything else.
pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,hyper=warn,tower_http=info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true))
        .init();
}
