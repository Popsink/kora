//! Kora — A lightweight, high-performance Schema Registry.

use kora::{api, config::KoraConfig, storage};
use tokio::net::TcpListener;

// -- Entrypoint --

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = KoraConfig::load().expect("failed to load configuration");

    tracing::info!(host = %cfg.host, port = %cfg.port, "starting Kora");

    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    metrics::describe_counter!("http_requests_total", "Total HTTP requests served");
    metrics::describe_histogram!(
        "http_request_duration_seconds",
        "HTTP request latency in seconds"
    );
    metrics::describe_gauge!(
        "kora_schema_count",
        "Number of unique schema contents in the registry"
    );
    metrics::describe_gauge!(
        "kora_db_connections_in_use",
        "Database connections currently executing queries"
    );
    metrics::describe_gauge!(
        "kora_db_connections_idle",
        "Idle database connections in the pool"
    );

    let pool = storage::create_pool(&cfg.database_url, cfg.db_pool_max)
        .await
        .expect("failed to connect to database");

    // Reconcile the declaratively-configured default compatibility level into the
    // global config row: DEFAULT_COMPATIBILITY is the source of truth on every
    // startup, overwriting any runtime PUT/DELETE /config change to the global
    // level. Per-subject overrides are untouched.
    if let Some(applied) =
        storage::apply_startup_config(&pool, cfg.default_compatibility.as_deref())
            .await
            .expect("failed to reconcile global compatibility level")
    {
        tracing::info!(
            compatibility = %applied,
            "reconciled global compatibility from DEFAULT_COMPATIBILITY"
        );
    }

    let app = api::router(pool, metrics_handle, cfg.max_body_size);
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");

    tracing::info!(%addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

// -- Helpers --

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        tokio::select! {
            result = ctrl_c => { result.expect("failed to listen for CTRL+C"); },
            recv = sigterm.recv() => {
                if recv.is_none() {
                    tracing::warn!("SIGTERM stream closed unexpectedly");
                }
            },
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await.expect("failed to install CTRL+C handler");

    tracing::info!("shutdown signal received");
}
