//! Registry-mode operations for the `PostgreSQL` backend.
//!
//! Simple reads/writes run through the SQL toolkit helpers; the reset and
//! per-subject clear paths run in a transaction over the raw
//! [`PgPool`](sqlx::PgPool) so they can read the previous value and clear it
//! (and any orphan config rows) atomically.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::{scalar_opt_string, scalar_string};

use super::PgStorage;

pub(super) async fn get_global_mode(store: &PgStorage) -> Result<String, KoraError> {
    scalar_string(
        store,
        "SELECT COALESCE(mode, 'READWRITE') FROM config WHERE subject IS NULL",
        &[],
    )
    .await
}

pub(super) async fn set_global_mode(
    store: &PgStorage,
    mode_value: &str,
) -> Result<String, KoraError> {
    scalar_string(
        store,
        "UPDATE config SET mode = $1, updated_at = now() WHERE subject IS NULL RETURNING mode",
        &binds![mode_value],
    )
    .await
}

pub(super) async fn get_subject_mode(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    scalar_opt_string(
        store,
        "SELECT mode FROM config WHERE subject = $1 AND mode IS NOT NULL",
        &binds![subject],
    )
    .await
}

pub(super) async fn set_subject_mode(
    store: &PgStorage,
    subject: &str,
    mode_value: &str,
) -> Result<String, KoraError> {
    scalar_string(
        store,
        r"INSERT INTO config (subject, mode)
          VALUES ($1, $2)
          ON CONFLICT (subject) DO UPDATE SET mode = $2, updated_at = now()
          RETURNING mode",
        &binds![subject, mode_value],
    )
    .await
}

pub(super) async fn get_effective_mode(
    store: &PgStorage,
    subject: &str,
) -> Result<String, KoraError> {
    scalar_string(
        store,
        r"SELECT COALESCE(
            (SELECT mode FROM config WHERE subject = $1 AND mode IS NOT NULL),
            (SELECT COALESCE(mode, 'READWRITE') FROM config WHERE subject IS NULL)
          )",
        &binds![subject],
    )
    .await
}

// -- Transactional operations --

/// Reset the global registry mode to READWRITE (default).
///
/// Returns the **previous** mode before the reset.
pub(super) async fn delete_global_mode(store: &PgStorage) -> Result<String, KoraError> {
    let mut tx = store.pool().begin().await?;

    let prev_mode: String = sqlx::query_scalar(
        "SELECT COALESCE(mode, 'READWRITE') FROM config WHERE subject IS NULL FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("UPDATE config SET mode = 'READWRITE', updated_at = now() WHERE subject IS NULL")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(prev_mode)
}

/// Delete per-subject mode by setting it to NULL.
///
/// Returns the **previous** mode, or `None` if no per-subject mode was set.
/// Cleans up the config row if no other config remains.
pub(super) async fn delete_subject_mode(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let mut tx = store.pool().begin().await?;

    let prev = sqlx::query_scalar::<_, String>(
        "SELECT mode FROM config WHERE subject = $1 AND mode IS NOT NULL FOR UPDATE",
    )
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;

    if prev.is_some() {
        sqlx::query("UPDATE config SET mode = NULL, updated_at = now() WHERE subject = $1")
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
    Ok(prev)
}

/// Delete per-subject mode for a subject and all child subjects (prefix match), atomically.
///
/// Returns the parent's **previous** mode, or `None` if no per-subject mode was set.
/// Uses the `^@` (starts-with) operator instead of LIKE to avoid wildcard injection.
pub(super) async fn delete_subject_mode_recursive(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let mut tx = store.pool().begin().await?;

    // Read parent's current mode.
    let prev = sqlx::query_scalar::<_, String>(
        "SELECT mode FROM config WHERE subject = $1 AND mode IS NOT NULL FOR UPDATE",
    )
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;

    // Clear mode on parent.
    if prev.is_some() {
        sqlx::query("UPDATE config SET mode = NULL, updated_at = now() WHERE subject = $1")
            .bind(subject)
            .execute(&mut *tx)
            .await?;
    }

    // Clear mode on all child subjects (prefix match via ^@ operator).
    sqlx::query(
        "UPDATE config SET mode = NULL, updated_at = now() WHERE subject ^@ $1 AND subject != $1 AND mode IS NOT NULL",
    )
    .bind(subject)
    .execute(&mut *tx)
    .await?;

    // Clean up orphan rows (parent + children).
    sqlx::query(
        "DELETE FROM config WHERE (subject = $1 OR (subject ^@ $1 AND subject != $1)) AND compatibility_level IS NULL AND mode IS NULL",
    )
    .bind(subject)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(prev)
}
