//! Generic query helpers built on [`SqlExecutor`].
//!
//! These collapse the bulk of the single-statement `Storage` methods to one
//! line each: the backend supplies its dialect SQL string and binds, the helper
//! runs it and extracts the result. They are generic functions (monomorphized
//! per backend) — no `dyn`, no vtable.

use super::{Bind, Row, SqlExecutor};
use crate::error::KoraError;

/// First column of the single result row as `i64`; `0` when there is no row
/// (a `COUNT` always returns a row, so this is the empty-aggregate guard).
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn scalar_i64<E: SqlExecutor>(e: &E, sql: &str, p: &[Bind]) -> Result<i64, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => row.get_i64(0),
        None => Ok(0),
    }
}

/// First column of the single result row as a nullable `i64` (e.g. `MAX(id)`).
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn scalar_opt_i64<E: SqlExecutor>(
    e: &E,
    sql: &str,
    p: &[Bind],
) -> Result<Option<i64>, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => row.get_opt_i64(0),
        None => Ok(None),
    }
}

/// First column of the single result row as text, or `None` when absent/NULL.
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn scalar_opt_string<E: SqlExecutor>(
    e: &E,
    sql: &str,
    p: &[Bind],
) -> Result<Option<String>, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => row.get_opt_str(0),
        None => Ok(None),
    }
}

/// First column of the single result row as text. Use for `COALESCE`d queries
/// that always return exactly one non-null row.
///
/// # Errors
/// Returns [`KoraError::BackendDataStore`] if no row came back; propagates decode errors.
pub async fn scalar_string<E: SqlExecutor>(
    e: &E,
    sql: &str,
    p: &[Bind],
) -> Result<String, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => row.get_str(0),
        None => Err(KoraError::BackendDataStore(
            "expected exactly one row".to_owned(),
        )),
    }
}

/// First column of the single result row as a bool; `false` when there is no row.
///
/// The backend supplies its own existence SQL (Postgres `SELECT EXISTS(..)`,
/// Oracle `SELECT CASE WHEN EXISTS(..) THEN 1 ELSE 0 END FROM dual`); reading the
/// column as a bool is backend-blind because each `Row::get_bool` normalizes its
/// native boolean / `NUMBER(1)` representation.
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn scalar_bool<E: SqlExecutor>(e: &E, sql: &str, p: &[Bind]) -> Result<bool, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => row.get_bool(0),
        None => Ok(false),
    }
}

/// First column of the single result row as a nullable bool (e.g. a per-subject
/// `normalize` override that may be absent).
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn scalar_opt_bool<E: SqlExecutor>(
    e: &E,
    sql: &str,
    p: &[Bind],
) -> Result<Option<bool>, KoraError> {
    match e.fetch_optional(sql, p).await? {
        Some(row) => Ok(Some(row.get_bool(0)?)),
        None => Ok(None),
    }
}

/// Collect the first (text) column of an ordered, windowed query into a `Vec`.
///
/// # Errors
/// Propagates query failures and column-decode errors.
pub async fn fetch_strings<E: SqlExecutor>(
    e: &E,
    base_sql: &str,
    p: &[Bind],
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    e.fetch_all_paged(base_sql, p, offset, limit)
        .await?
        .iter()
        .map(|r| r.get_str(0))
        .collect()
}
