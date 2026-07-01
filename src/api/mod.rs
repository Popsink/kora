//! API route construction.

pub mod compatibility;
pub mod health;
pub mod metrics;
mod middleware;
pub mod mode;
pub mod schemas;
pub mod subjects;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef},
    response::IntoResponse,
    routing::{get, post},
};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::storage::DynStorage;

// -- State --

/// Shared application state extracted by handlers via axum's `State`.
///
/// `FromRef` lets handlers extract `State<DynStorage>` (the backend-agnostic
/// storage handle) and `State<PrometheusHandle>` directly.
#[derive(Clone, FromRef)]
struct AppState {
    storage: DynStorage,
    metrics_handle: PrometheusHandle,
}

// -- Router --

/// Root endpoint — returns empty JSON object (Confluent compatibility).
async fn root() -> impl IntoResponse {
    Json(serde_json::json!({}))
}

/// Build the application router with all routes.
pub fn router(
    storage: DynStorage,
    metrics_handle: PrometheusHandle,
    max_body_size: usize,
) -> Router {
    let state = AppState {
        storage,
        metrics_handle,
    };

    // API routes get the Confluent content-type header.
    let api = Router::new()
        .route("/", get(root).post(root))
        .route("/health", get(health::check_health))
        .route("/schemas", get(schemas::list_schemas))
        .route("/schemas/ids/{id}", get(schemas::get_schema_by_id))
        .route(
            "/schemas/ids/{id}/schema",
            get(schemas::get_schema_text_by_id),
        )
        .route(
            "/schemas/ids/{id}/subjects",
            get(schemas::get_subjects_by_schema_id),
        )
        .route(
            "/schemas/ids/{id}/versions",
            get(schemas::get_versions_by_schema_id),
        )
        .route("/schemas/types", get(schemas::list_schema_types))
        .route("/subjects", get(subjects::list_subjects))
        .route(
            "/subjects/{subject}",
            post(subjects::check_schema).delete(subjects::delete_subject),
        )
        .route(
            "/subjects/{subject}/versions",
            get(subjects::list_versions).post(subjects::register_schema),
        )
        .route(
            "/subjects/{subject}/versions/{version}",
            get(subjects::get_schema_by_version).delete(subjects::delete_version),
        )
        .route(
            "/subjects/{subject}/versions/{version}/schema",
            get(subjects::get_schema_text_by_version),
        )
        .route(
            "/subjects/{subject}/versions/{version}/referencedby",
            get(subjects::get_referencing_ids_by_version),
        )
        .route(
            "/compatibility/subjects/{subject}/versions",
            post(compatibility::test_compatibility_against_all_versions),
        )
        .route(
            "/compatibility/subjects/{subject}/versions/{version}",
            post(compatibility::test_compatibility_by_version),
        )
        .route(
            "/config",
            get(compatibility::get_global_compatibility)
                .put(compatibility::set_global_compatibility)
                .delete(compatibility::delete_global_compatibility),
        )
        .route(
            "/config/{subject}",
            get(compatibility::get_subject_compatibility)
                .put(compatibility::set_subject_compatibility)
                .delete(compatibility::delete_subject_compatibility),
        )
        .route(
            "/mode",
            get(mode::get_global_mode)
                .put(mode::set_global_mode)
                .delete(mode::delete_global_mode),
        )
        .route(
            "/mode/{subject}",
            get(mode::get_subject_mode)
                .put(mode::set_subject_mode)
                .delete(mode::delete_subject_mode),
        )
        .layer(axum::middleware::from_fn(
            middleware::content_type_negotiation,
        ))
        .layer(axum::middleware::from_fn(middleware::normalize_query_bools));

    // Operational routes — no Confluent content-type header.
    let ops = Router::new().route("/metrics", get(metrics::get_metrics));

    api.merge(ops)
        .layer(axum::middleware::from_fn(middleware::track_metrics))
        .layer(DefaultBodyLimit::max(max_body_size))
        .with_state(state)
}
