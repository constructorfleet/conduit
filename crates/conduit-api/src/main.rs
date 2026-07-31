//! Conduit API server.

use std::net::SocketAddr;

use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("CONDUIT_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .init();

    let addr: SocketAddr =
        std::env::var("CONDUIT_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_owned()).parse()?;

    let state = AppState::new(EventBus::default());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "conduit api listening");

    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

/// Resolves on SIGINT or SIGTERM so in-flight requests can drain.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::warn!(%error, "cannot listen for SIGTERM"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!("shutdown signal received");
}
