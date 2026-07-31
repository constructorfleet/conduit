//! Conduit API server.

use std::net::SocketAddr;

use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tracer_provider = init_tracing()?;

    let addr: SocketAddr =
        std::env::var("CONDUIT_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_owned()).parse()?;

    let (providers, registered) = conduit_api::config::from_env()?;
    for description in &registered.descriptions {
        tracing::info!(provider = %description, "registered provider");
    }

    let store = conduit_api::config::store_from_env().await?;
    let mut state = AppState::with_store(EventBus::default(), store);
    if !registered.is_empty() {
        state = state.with_providers(providers);
    }
    let state = with_dev_providers(state);
    conduit_metrics::Collector::spawn(state.metrics(), &state.bus);
    if state.providers().is_none() {
        tracing::warn!(
            "no providers are configured; conversations will be refused until \
             CONDUIT_OPENAI_BASE_URL or CONDUIT_OPENAI_API_KEY is set"
        );
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "conduit api listening");

    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await?;
    if let Some(provider) = tracer_provider {
        provider.shutdown()?;
    }
    Ok(())
}

/// Sets up structured logs and, when configured, OTLP span export.
fn init_tracing() -> Result<Option<SdkTracerProvider>, Box<dyn std::error::Error>> {
    let filter =
        EnvFilter::try_from_env("CONDUIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer().json();

    if otlp_enabled() {
        let exporter = opentelemetry_otlp::SpanExporter::builder().with_http().build()?;
        let provider = SdkTracerProvider::builder().with_batch_exporter(exporter).build();
        let tracer = provider.tracer("conduit-api");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        tracing_subscriber::registry().with(filter).with(fmt_layer).with(otel_layer).init();
        return Ok(Some(provider));
    }

    tracing_subscriber::registry().with(filter).with(fmt_layer).init();
    Ok(None)
}

/// Whether a collector endpoint was configured for trace export.
fn otlp_enabled() -> bool {
    ["OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "OTEL_EXPORTER_OTLP_ENDPOINT"]
        .into_iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

/// Registers the providers this build was compiled with.
///
/// A server with none still serves the whole API except conversations, which
/// is the honest default: nothing here should pretend to hear speech unless
/// someone asked it to.
#[cfg(feature = "dev-providers")]
fn with_dev_providers(state: AppState) -> AppState {
    use conduit_provider::testing::{EchoLlm, EchoStt, EchoTts};
    use conduit_runtime::Providers;

    tracing::warn!(
        "the echo providers are enabled; this build transcribes audio as text \
         and cannot hear speech"
    );
    state.with_providers(Providers::new().with_stt(EchoStt).with_llm(EchoLlm).with_tts(EchoTts))
}

/// Registers no providers; conversations are refused until some are configured.
#[cfg(not(feature = "dev-providers"))]
fn with_dev_providers(state: AppState) -> AppState {
    state
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
