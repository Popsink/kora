//! Shared test helpers for integration tests.
// Each test file compiles this module independently, so unused helpers
// produce false-positive warnings.
#![allow(dead_code)]

pub mod api;

use std::sync::OnceLock;

use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;

// -- Constants --

/// Pass to list helpers to include soft-deleted items.
pub const INCLUDE_DELETED: bool = true;

/// Pass to list helpers to show only active items.
pub const ACTIVE_ONLY: bool = false;

// -- Fixtures --

/// A simple valid Avro schema with one field.
pub const AVRO_SCHEMA_V1: &str =
    r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"}]}"#;

/// A valid Avro schema with two fields — backward-compatible with V1 (optional name with default).
pub const AVRO_SCHEMA_V2: &str = r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"},{"name":"name","type":["null","string"],"default":null}]}"#;

/// A valid Avro schema with three fields — backward-compatible with V2 (optional active with default).
pub const AVRO_SCHEMA_V3: &str = r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"},{"name":"name","type":["null","string"],"default":null},{"name":"active","type":["null","boolean"],"default":null}]}"#;

/// A valid Avro schema with a different record name (for check-schema tests).
pub const AVRO_SCHEMA_OTHER: &str =
    r#"{"type":"record","name":"Other","fields":[{"name":"x","type":"string"}]}"#;

// -- JSON Schema Fixtures --

/// A valid JSON Schema with one property.
pub const JSON_SCHEMA_V1: &str = r#"{"type":"object","properties":{"name":{"type":"string"}}}"#;

/// A valid JSON Schema with two properties (different from V1).
pub const JSON_SCHEMA_V2: &str =
    r#"{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}}}"#;

// -- Protobuf Fixtures --

/// A valid Protobuf schema with one field.
pub const PROTO_SCHEMA_V1: &str = "syntax = \"proto3\";\nmessage Test {\n  int32 id = 1;\n}";

/// A valid Protobuf schema with two fields (different from V1).
pub const PROTO_SCHEMA_V2: &str =
    "syntax = \"proto3\";\nmessage Test {\n  int32 id = 1;\n  string name = 2;\n}";

// -- Compatibility test fixtures (proper Avro evolution with defaults) --

/// Avro record with one required field — base for compatibility testing.
pub const COMPAT_AVRO_V1: &str =
    r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"}]}"#;

/// Backward-compatible evolution: adds optional field with null default.
pub const COMPAT_AVRO_V2: &str = r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"},{"name":"name","type":["null","string"],"default":null}]}"#;

/// Backward-compatible evolution of V2: adds another optional field.
pub const COMPAT_AVRO_V3: &str = r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"},{"name":"name","type":["null","string"],"default":null},{"name":"active","type":["null","boolean"],"default":null}]}"#;

/// Backward-INCOMPATIBLE schema: adds required field without default.
pub const COMPAT_AVRO_INCOMPAT: &str = r#"{"type":"record","name":"Test","fields":[{"name":"id","type":"int"},{"name":"email","type":"string"}]}"#;

// -- Helpers --

/// Generate a unique Avro schema (unique record name → unique fingerprint).
/// Use this in tests that depend on cross-ref isolation (global content dedup
/// means shared schema text shares a content ID across all tests).
pub fn unique_avro_schema() -> String {
    format!(
        r#"{{"type":"record","name":"T{}","fields":[{{"name":"id","type":"int"}}]}}"#,
        uuid::Uuid::new_v4().as_simple()
    )
}

// -- Setup --

/// Get `DATABASE_URL` from env. Panics if not set — use `just test` to run.
pub fn database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set — run via `just test`")
}

/// True when the configured backend is Oracle. White-box tests that issue raw
/// `PostgreSQL` SQL against [`pool`] guard on this and skip on Oracle; the
/// black-box HTTP tests run against whichever backend is configured.
pub fn backend_is_oracle() -> bool {
    if std::env::var("DB_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("oracle")) {
        return true;
    }
    std::env::var("DATABASE_URL").is_ok_and(|v| v.starts_with("oracle:"))
}

/// Create a `PostgreSQL` pool with migrations applied — for `PostgreSQL`-specific
/// white-box tests that query the schema directly.
pub async fn pool() -> sqlx::PgPool {
    kora::storage::create_pool(&database_url(), 10)
        .await
        .expect("database should be reachable")
}

/// Build the configured storage backend (Postgres or Oracle). Backend-agnostic —
/// used by the black-box HTTP tests.
///
/// Each test gets its **own** pool: every `#[tokio::test]` runs on its own tokio
/// runtime, and a `sqlx` pool is bound to the runtime that created it — sharing one
/// pool across tests breaks once the creating test's runtime is dropped. A per-test
/// pool sidesteps that for both backends. Migrations run **once per process**
/// (guarded by [`MIGRATED`]): the schema is shared and cargo runs test binaries
/// sequentially, so this avoids dozens of concurrent `migrate()` calls.
///
/// Oracle's thick driver serialises session-pool *creation* internally (see
/// `OracleStorage::connect`), so many per-test pools no longer race on
/// `OCISessionPoolCreate` (`ORA-24416`).
pub async fn storage() -> kora::storage::DynStorage {
    let mut cfg = kora::config::KoraConfig::load().expect("config should load from env");
    // Each test spawns its own server (and pool); keep pools small so the parallel
    // suite does not exhaust the database's connection limit.
    cfg.db_pool_max = cfg.db_pool_max.min(5);
    let store = kora::storage::connect(&cfg)
        .await
        .expect("storage backend should connect");
    MIGRATED
        .get_or_init(|| async {
            store.migrate().await.expect("migrations should apply");
        })
        .await;
    store
}

/// Ensures migrations are applied exactly once per test process.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Return a process-wide `PrometheusHandle`, installing the recorder on first call.
///
/// `install_recorder()` panics when called twice, so the handle is cached
/// in a `OnceLock` to guarantee a single installation per test process.
fn prometheus_handle() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install Prometheus recorder");

            // Mirror the describe_*! calls from main.rs so tests get HELP/TYPE metadata.
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

            handle
        })
        .clone()
}

/// Spawn the Kora server on a random port and return the base URL.
pub async fn spawn_server() -> String {
    let storage = storage().await;
    let config = kora::config::KoraConfig::default();
    let app = kora::api::router(storage, prometheus_handle(), config.max_body_size);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind to random port");
    let addr = listener.local_addr().expect("should have local addr");
    let base = format!("http://127.0.0.1:{}", addr.port());

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });

    base
}
