//! Compatibility-config operations for the `PostgreSQL` backend.
//!
//! Simple reads/writes run single statements over the pool; the two reset paths
//! run in a transaction over the raw [`PgPool`](sqlx::PgPool) so they can read the
//! previous value and clear it atomically.

use crate::error::KoraError;

use super::PgStorage;

pub(super) async fn get_subject_level(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    Ok(sqlx::query_scalar(
        "SELECT compatibility_level FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL",
    )
    .bind(subject)
    .fetch_optional(store.pool())
    .await?)
}

pub(super) async fn get_global_level(store: &PgStorage) -> Result<String, KoraError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(compatibility_level, 'BACKWARD') FROM config WHERE subject IS NULL",
    )
    .fetch_one(store.pool())
    .await?)
}

pub(super) async fn set_global_level(
    store: &PgStorage,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    Ok(sqlx::query_scalar(
        r"UPDATE config SET compatibility_level = $1, normalize = $2, updated_at = now()
          WHERE subject IS NULL
          RETURNING compatibility_level",
    )
    .bind(level)
    .bind(normalize)
    .fetch_one(store.pool())
    .await?)
}

pub(super) async fn reconcile_global_level(
    store: &PgStorage,
    level: &str,
) -> Result<String, KoraError> {
    Ok(sqlx::query_scalar(
        r"UPDATE config SET compatibility_level = $1, updated_at = now()
          WHERE subject IS NULL
          RETURNING compatibility_level",
    )
    .bind(level)
    .fetch_one(store.pool())
    .await?)
}

pub(super) async fn set_subject_level(
    store: &PgStorage,
    subject: &str,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    Ok(sqlx::query_scalar(
        r"INSERT INTO config (subject, compatibility_level, normalize)
          VALUES ($1, $2, $3)
          ON CONFLICT (subject) DO UPDATE SET compatibility_level = $2, normalize = $3, updated_at = now()
          RETURNING compatibility_level",
    )
    .bind(subject)
    .bind(level)
    .bind(normalize)
    .fetch_one(store.pool())
    .await?)
}

pub(super) async fn get_global_normalize(store: &PgStorage) -> Result<bool, KoraError> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(normalize, false) FROM config WHERE subject IS NULL")
            .fetch_optional(store.pool())
            .await?
            .unwrap_or(false),
    )
}

pub(super) async fn get_subject_normalize(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<bool>, KoraError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(normalize, false) FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL",
    )
    .bind(subject)
    .fetch_optional(store.pool())
    .await?)
}

// -- Transactional operations --

/// Delete per-subject compatibility config by setting it to NULL.
///
/// Returns the **previous** `(level, normalize)`, or `None` if not configured.
pub(super) async fn delete_subject_level(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<(String, bool)>, KoraError> {
    let mut tx = store.pool().begin().await?;

    let row = sqlx::query(
        "SELECT compatibility_level, COALESCE(normalize, false) AS normalize FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL FOR UPDATE",
    )
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;

    let result = row.map(|r| {
        let level: String = sqlx::Row::get(&r, "compatibility_level");
        let normalize: bool = sqlx::Row::get(&r, "normalize");
        (level, normalize)
    });

    if result.is_some() {
        // Reset compat fields to NULL; delete row if mode is also NULL.
        sqlx::query(
            "UPDATE config SET compatibility_level = NULL, normalize = NULL, updated_at = now() WHERE subject = $1",
        )
        .bind(subject)
        .execute(&mut *tx)
        .await?;

        // Clean up orphan row (all nullable fields are NULL).
        sqlx::query(
            "DELETE FROM config WHERE subject = $1 AND compatibility_level IS NULL AND mode IS NULL",
        )
        .bind(subject)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(result)
}

/// Delete (reset) the global compatibility level to BACKWARD (default).
///
/// Returns the **previous** `(compatibility_level, normalize)` before the reset.
pub(super) async fn delete_global_level(store: &PgStorage) -> Result<(String, bool), KoraError> {
    let mut tx = store.pool().begin().await?;

    let row = sqlx::query(
        "SELECT COALESCE(compatibility_level, 'BACKWARD') AS compatibility_level, COALESCE(normalize, false) AS normalize FROM config WHERE subject IS NULL FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;

    let prev_level: String = sqlx::Row::get(&row, "compatibility_level");
    let prev_normalize: bool = sqlx::Row::get(&row, "normalize");

    sqlx::query(
        "UPDATE config SET compatibility_level = 'BACKWARD', normalize = false, updated_at = now() WHERE subject IS NULL",
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((prev_level, prev_normalize))
}
