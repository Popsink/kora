//! Subject operations for the `PostgreSQL` backend.
//!
//! Simple reads run through the SQL toolkit helpers; the two delete paths run in
//! a transaction over the raw [`PgPool`](sqlx::PgPool) and own their dialect SQL
//! verbatim.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::{fetch_strings, scalar_bool, scalar_opt_i64};
use crate::storage::types::HardDeleteResult;

use super::{PgStorage, like_pattern};

pub(super) async fn list_subjects(
    store: &PgStorage,
    include_deleted: bool,
    deleted_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    // Literal WHERE filter (one of three hardcoded literals) so PG can use its
    // partial indexes; binding the deleted flag here would prevent index usage.
    let filter = if deleted_only {
        "deleted = true"
    } else if include_deleted {
        "true"
    } else {
        "deleted = false"
    };
    if let Some(pat) = like_pattern(prefix) {
        let sql = format!(
            "SELECT name FROM subjects WHERE {filter} AND name LIKE $1 ESCAPE '\\' ORDER BY name"
        );
        fetch_strings(store, &sql, &binds![pat], offset, limit).await
    } else {
        let sql = format!("SELECT name FROM subjects WHERE {filter} ORDER BY name");
        fetch_strings(store, &sql, &[], offset, limit).await
    }
}

pub(super) async fn find_subject_id_by_name(
    store: &PgStorage,
    name: &str,
    include_deleted: bool,
) -> Result<Option<i64>, KoraError> {
    scalar_opt_i64(
        store,
        "SELECT id FROM subjects WHERE name = $1 AND (deleted = false OR $2)",
        &binds![name, include_deleted],
    )
    .await
}

pub(super) async fn subject_exists(
    store: &PgStorage,
    name: &str,
    include_deleted: bool,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE name = $1 AND (deleted = false OR $2))",
        &binds![name, include_deleted],
    )
    .await
}

pub(super) async fn subject_is_soft_deleted(
    store: &PgStorage,
    name: &str,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE name = $1 AND deleted = true)",
        &binds![name],
    )
    .await
}

// -- Transactional operations --

/// Soft-delete a subject and all its schema versions. Returns the deleted version
/// numbers sorted ascending. Runs in a transaction for consistency.
pub(super) async fn soft_delete_subject(
    store: &PgStorage,
    name: &str,
) -> Result<Vec<i32>, KoraError> {
    let mut tx = store.pool().begin().await?;

    let mut versions = sqlx::query_scalar::<_, i32>(
        r"UPDATE schema_versions SET deleted = true
           WHERE subject_id = (SELECT id FROM subjects WHERE name = $1) AND deleted = false
           RETURNING version",
    )
    .bind(name)
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query("UPDATE subjects SET deleted = true WHERE name = $1 AND deleted = false")
        .bind(name)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    versions.sort_unstable();
    Ok(versions)
}

/// Hard-delete a subject atomically: lock the row, verify preconditions
/// (must be soft-deleted, no referenced versions), then delete.
///
/// All checks run inside the transaction to eliminate TOCTOU races
/// with concurrent writers that could re-activate the subject.
pub(super) async fn hard_delete_subject(
    store: &PgStorage,
    name: &str,
) -> Result<HardDeleteResult, KoraError> {
    let mut tx = store.pool().begin().await?;

    // Lock the subject row to prevent concurrent modifications.
    let Some((subject_id, deleted)) = sqlx::query_as::<_, (i64, bool)>(
        "SELECT id, deleted FROM subjects WHERE name = $1 FOR UPDATE",
    )
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(HardDeleteResult::NotFound);
    };

    if !deleted {
        return Ok(HardDeleteResult::NotSoftDeleted);
    }

    // Check references inside the transaction (no TOCTOU).
    let versions: Vec<i32> = sqlx::query_scalar(
        "SELECT version FROM schema_versions WHERE subject_id = $1 AND deleted = true",
    )
    .bind(subject_id)
    .fetch_all(&mut *tx)
    .await?;

    for v in &versions {
        let is_referenced: bool = sqlx::query_scalar(
            r"SELECT EXISTS(
                SELECT 1 FROM schema_references sr
                JOIN schema_versions sv ON sr.content_id = sv.content_id
                WHERE sr.subject = $1 AND sr.version = $2 AND sv.deleted = false
            )",
        )
        .bind(name)
        .bind(v)
        .fetch_one(&mut *tx)
        .await?;

        if is_referenced {
            return Ok(HardDeleteResult::ReferenceExists(format!(
                "{name} version {v}"
            )));
        }
    }

    // Delete soft-deleted versions.
    sqlx::query("DELETE FROM schema_versions WHERE subject_id = $1 AND deleted = true")
        .bind(subject_id)
        .execute(&mut *tx)
        .await?;

    // Only delete the subject if no active versions remain (a concurrent writer
    // may have inserted a new version between our lock and this point via the
    // UPSERT which re-activates the subject).
    let has_active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM schema_versions WHERE subject_id = $1 AND deleted = false)",
    )
    .bind(subject_id)
    .fetch_one(&mut *tx)
    .await?;

    if !has_active {
        sqlx::query("DELETE FROM subjects WHERE id = $1")
            .bind(subject_id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;

    let mut sorted = versions;
    sorted.sort_unstable();
    Ok(HardDeleteResult::Deleted(sorted))
}
