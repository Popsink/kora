//! Prometheus metrics endpoint.

use std::time::Duration;

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::storage::DynStorage;

// -- Handlers --

/// `GET /metrics` — Prometheus text exposition format.
///
/// Snapshots business and pool gauges at scrape time so values are
/// always fresh when the Prometheus scraper calls.
pub async fn get_metrics(
    State(storage): State<DynStorage>,
    State(handle): State<PrometheusHandle>,
) -> Response {
    // Snapshot business metrics.  On DB failure or slow query the gauge
    // keeps its previous value rather than blocking the entire scrape.
    if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), storage.schema_count()).await {
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!("kora_schema_count").set(n as f64);
    }

    // Snapshot connection pool metrics.
    // size and idle are sampled non-atomically — clamp to avoid a negative gauge.
    let stats = storage.pool_stats();
    let idle = f64::from(stats.idle);
    let size = f64::from(stats.size);
    metrics::gauge!("kora_db_connections_in_use").set((size - idle).max(0.0));
    metrics::gauge!("kora_db_connections_idle").set(idle);

    let body = handle.render();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}
