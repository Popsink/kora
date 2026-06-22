//! `PostgreSQL` storage layer.

pub mod compatibility;
pub mod mode;
pub mod references;
pub mod schemas;
pub mod subjects;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

// -- Pool --

/// Create a connection pool and run embedded migrations.
///
/// # Errors
///
/// Returns an error if the database is unreachable or migrations fail.
pub async fn create_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

// -- Startup reconciliation --

/// Apply declarative startup configuration to the database. Idempotent and safe
/// to run on every boot.
///
/// When `default_compatibility` is set, reconciles the global compatibility level
/// (`subject IS NULL`) to that value and returns it (for the caller to log).
/// When `None`, it is a no-op returning `None`. Per-subject overrides are never
/// touched.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn apply_startup_config(
    pool: &PgPool,
    default_compatibility: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    match default_compatibility {
        Some(level) => Ok(Some(
            compatibility::reconcile_global_level(pool, level).await?,
        )),
        None => Ok(None),
    }
}
