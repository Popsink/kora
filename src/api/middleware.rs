//! Shared middleware layers for the API.

use std::time::Instant;

use axum::{
    extract::MatchedPath,
    http::{Request, Uri, header},
    middleware::Next,
    response::IntoResponse,
};

use crate::api::auth::PrincipalLabel;
use crate::error::CONTENT_TYPE_SCHEMA_REGISTRY;

/// Middleware that normalizes case-insensitive boolean values in query strings.
///
/// The Confluent Python client sends `True`/`False` (Python's `str(bool)`)
/// but serde only accepts lowercase. This rewrites the URI once so every
/// endpoint handles it transparently.
pub async fn normalize_query_bools(
    mut req: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    if let Some(query) = req.uri().query() {
        let normalized: String = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((k, v)) if v.eq_ignore_ascii_case("true") => format!("{k}=true"),
                Some((k, v)) if v.eq_ignore_ascii_case("false") => format!("{k}=false"),
                _ => pair.to_string(),
            })
            .collect::<Vec<_>>()
            .join("&");

        if normalized != query {
            let pq = format!("{}?{normalized}", req.uri().path());
            if let Ok(uri) = pq.parse::<Uri>() {
                *req.uri_mut() = uri;
            }
        }
    }
    next.run(req).await
}

// -- Content-type negotiation --

const CONTENT_TYPE_JSON: &str = "application/json";

/// Middleware that sets the response `Content-Type` based on the request `Accept`
/// header, matching real Confluent Schema Registry behaviour:
/// - If the client sends `Accept: application/vnd.schemaregistry.v1+json`,
///   respond with that content type.
/// - Otherwise, default to `application/json`.
pub async fn content_type_negotiation(
    req: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    let use_vendor = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains(CONTENT_TYPE_SCHEMA_REGISTRY));

    let mut response = next.run(req).await;

    let ct = if use_vendor {
        CONTENT_TYPE_SCHEMA_REGISTRY
    } else {
        CONTENT_TYPE_JSON
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, ct.parse().unwrap());

    response
}

// -- Request instrumentation --

/// Middleware that logs every request and records Prometheus metrics.
///
/// Emits a structured log line per request using field names aligned with
/// [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/):
/// `http.method`, `http.route`, `http.status_code`, `url.path`, `latency_ms`,
/// plus `principal` when authentication is enabled (never the password).
///
/// `401`/`403` responses also increment `kora_auth_rejections_total`, labelled
/// by status and principal.
///
/// Log levels: DEBUG for `/metrics` and `/health`, ERROR for 5xx, INFO for
/// everything else (including 4xx — client errors are expected traffic).
pub async fn track_metrics(req: Request<axum::body::Body>, next: Next) -> impl IntoResponse {
    let method = req.method().to_string();
    let uri = req.uri().path().to_owned();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |mp| mp.as_str().to_owned());

    let start = Instant::now();
    let response = next.run(req).await;

    let elapsed = start.elapsed().as_secs_f64();
    let status_code = response.status().as_u16();
    let latency_ms = (elapsed * 1_000_000.0).round() / 1000.0;
    let principal = response
        .extensions()
        .get::<PrincipalLabel>()
        .map(|p| p.0.clone());

    // Log every request with OTel-aligned field names.
    // 5xx arm first so a failing /health is logged as ERROR, not DEBUG.
    match (route.as_str(), status_code) {
        (_, 500..) => {
            tracing::error!(
                http.method = %method,
                http.route = %route,
                url.path = %uri,
                http.status_code = status_code,
                principal = principal.as_deref(),
                latency_ms,
                "request"
            );
        }
        ("/metrics" | "/health", _) => {
            tracing::debug!(
                http.method = %method,
                http.route = %route,
                url.path = %uri,
                http.status_code = status_code,
                principal = principal.as_deref(),
                latency_ms,
                "request"
            );
        }
        _ => {
            tracing::info!(
                http.method = %method,
                http.route = %route,
                url.path = %uri,
                http.status_code = status_code,
                principal = principal.as_deref(),
                latency_ms,
                "request"
            );
        }
    }

    // Skip metrics self-instrumentation for the metrics endpoint.
    if route == "/metrics" {
        return response;
    }

    let status_str = status_code.to_string();

    if matches!(status_code, 401 | 403) {
        metrics::counter!(
            "kora_auth_rejections_total",
            "status" => status_str.clone(),
            "principal" => principal.unwrap_or_else(|| "anonymous".to_owned()),
        )
        .increment(1);
    }

    metrics::counter!(
        "http_requests_total",
        "method" => method.clone(),
        "path" => route.clone(),
        "status" => status_str,
    )
    .increment(1);

    metrics::histogram!(
        "http_request_duration_seconds",
        "method" => method,
        "path" => route,
    )
    .record(elapsed);

    response
}
