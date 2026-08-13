//! Conduit API server.

use std::net::SocketAddr;

use conduit_api::{ops_router, router, AppState};
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
    let ops_addr: SocketAddr = std::env::var("CONDUIT_OPS_BIND")
        .unwrap_or_else(|_| "0.0.0.0:9090".to_owned())
        .parse()?;

    // Read before anything else is built: a server that cannot say who may call
    // it should fail now, not after opening a port.
    let access = conduit_api::config::access_from_env().await?;
    // Also before the port: a malformed dashboard URL should stop the server
    // rather than wait to surprise whoever first clicks flash.
    let esphome = conduit_api::config::esphome_from_env()?;

    let (providers, registered) = conduit_api::config::from_env()?;
    let turn_history_retention =
        conduit_api::config::turn_history_retention_from_vars(&std::env::vars().collect())?;
    for description in &registered.descriptions {
        tracing::info!(provider = %description, "registered provider");
    }

    let pipeline_store = conduit_api::config::store_from_env().await?;
    let provider_store = conduit_api::config::provider_definition_store_from_env().await?;
    let speaker_store = conduit_api::config::speaker_roster_store_from_env().await?;
    let linked_service_store = conduit_api::config::linked_service_store_from_env().await?;
    let mut state = AppState::with_stores(EventBus::default(), pipeline_store, provider_store)
        .with_speaker_roster(speaker_store)
        .with_linked_service_store(linked_service_store)
        .with_access(access)
        .with_turn_idle_timeout(registered.turn_idle_timeout)
        .with_turn_history_retention(turn_history_retention);
    if let Some(dashboard) = esphome {
        tracing::info!(dashboard = %dashboard.base_url(), "ESPHome hand-off configured");
        state = state.with_esphome(dashboard);
    }
    if !registered.is_empty() {
        state = state.with_providers(providers);
    }
    state.reload_provider_definitions().await?;
    let state = with_dev_providers(state);
    conduit_metrics::Collector::spawn(state.metrics(), &state.bus);
    conduit_api::status::StatusCollector::spawn(state.status(), &state.bus);
    if state.providers().is_none() {
        tracing::warn!(
            "no providers are configured; conversations will be refused until \
             Provider Definitions are saved through the management API"
        );
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let ops_listener = tokio::net::TcpListener::bind(ops_addr).await?;
    tracing::info!(%addr, "conduit api listening");
    tracing::info!(
        %ops_addr,
        "conduit ops listening; /health, /ready, and /metrics are unauthenticated, so do not \
         publish this port outside your trust boundary"
    );

    // One signal, both listeners: a shutdown that stopped only one would leave a
    // half-dead server answering probes.
    let (signal, _) = tokio::sync::broadcast::channel(1);
    let service_signal = wait_for(signal.subscribe());
    let ops_signal = wait_for(signal.subscribe());
    tokio::spawn(async move {
        shutdown().await;
        let _ = signal.send(());
    });

    // Spec 0005 §Reachability: probe every stored link once on startup so
    // an operator sees fresh reachability state after Conduit restarts,
    // rather than a stale `Unknown` on rows that already had a probe.
    // Non-blocking — a slow peer must not gate the listener coming up.
    let probe_state = state.clone();
    tokio::spawn(async move {
        conduit_api::linked_services::probe_all(&probe_state).await;
    });

    let service =
        axum::serve(listener, router(state.clone())).with_graceful_shutdown(service_signal);
    let ops = axum::serve(ops_listener, ops_router(state)).with_graceful_shutdown(ops_signal);

    // Either listener failing takes the process down: a server missing half its
    // surface is not a server anyone asked to run.
    tokio::try_join!(service, ops)?;

    if let Some(provider) = tracer_provider {
        provider.shutdown()?;
    }
    Ok(())
}

/// Resolves when the shutdown signal is sent, or the sender is dropped.
async fn wait_for(mut signal: tokio::sync::broadcast::Receiver<()>) {
    let _ = signal.recv().await;
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
