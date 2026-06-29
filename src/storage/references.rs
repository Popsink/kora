//! Schema reference storage operations.

use sqlx::PgPool;

use crate::error::KoraError;
use crate::types::SchemaReference;

// -- Queries --

/// Validate that all referenced schemas exist and are not soft-deleted.
///
/// # Errors
///
/// Returns `KoraError::ReferenceNotFound` if any referenced subject/version
/// does not exist or is soft-deleted.
pub async fn validate_references(pool: &PgPool, refs: &[SchemaReference]) -> Result<(), KoraError> {
    for r in refs {
        let exists = sqlx::query_scalar::<_, bool>(
            r"SELECT EXISTS(
                SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                WHERE sub.name = $1 AND sv.version = $2
                  AND sv.deleted = false AND sub.deleted = false
            )",
        )
        .bind(&r.subject)
        .bind(r.version)
        .fetch_one(pool)
        .await?;

        if !exists {
            return Err(KoraError::ReferenceNotFound(format!(
                "Schema reference not found: subject '{}' version {}",
                r.subject, r.version
            )));
        }
    }
    Ok(())
}

/// Find all references for a given schema content ID.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_references_by_schema_id(
    pool: &PgPool,
    content_id: i64,
) -> Result<Vec<SchemaReference>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT name, subject, version FROM schema_references WHERE content_id = $1 ORDER BY name",
    )
    .bind(content_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|row| SchemaReference {
            name: sqlx::Row::get(row, "name"),
            subject: sqlx::Row::get(row, "subject"),
            version: sqlx::Row::get(row, "version"),
        })
        .collect())
}

/// Find references for many schema content IDs in one query.
///
/// Returns `(content_id, reference)` pairs (ordered by content then name) so the
/// caller can group them — avoids an N+1 when listing many schemas.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_references_for_schema_ids(
    pool: &PgPool,
    content_ids: &[i64],
) -> Result<Vec<(i64, SchemaReference)>, sqlx::Error> {
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT content_id, name, subject, version FROM schema_references \
         WHERE content_id = ANY($1) ORDER BY content_id, name",
    )
    .bind(content_ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|row| {
            (
                sqlx::Row::get::<i64, _>(row, "content_id"),
                SchemaReference {
                    name: sqlx::Row::get(row, "name"),
                    subject: sqlx::Row::get(row, "subject"),
                    version: sqlx::Row::get(row, "version"),
                },
            )
        })
        .collect())
}

/// Find content IDs of schemas that reference the given subject/version.
///
/// Returns the global schema IDs (content IDs) of schemas containing a
/// reference to `target_subject` at `target_version`. When `include_deleted`
/// is false, only content IDs with at least one active (non-deleted)
/// `schema_versions` row are returned.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_referencing_schema_ids(
    pool: &PgPool,
    target_subject: &str,
    target_version: i32,
    include_deleted: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i64>, sqlx::Error> {
    if limit >= 0 {
        sqlx::query_scalar(
            r"SELECT DISTINCT sr.content_id
               FROM schema_references sr
               JOIN schema_versions sv ON sr.content_id = sv.content_id
               WHERE sr.subject = $1 AND sr.version = $2
                 AND (sv.deleted = false OR $3)
               ORDER BY sr.content_id
               OFFSET $4 LIMIT $5",
        )
        .bind(target_subject)
        .bind(target_version)
        .bind(include_deleted)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_scalar(
            r"SELECT DISTINCT sr.content_id
               FROM schema_references sr
               JOIN schema_versions sv ON sr.content_id = sv.content_id
               WHERE sr.subject = $1 AND sr.version = $2
                 AND (sv.deleted = false OR $3)
               ORDER BY sr.content_id
               OFFSET $4",
        )
        .bind(target_subject)
        .bind(target_version)
        .bind(include_deleted)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
}

/// Check if a subject/version is referenced by any **active** (non-deleted) schema version.
///
/// Joins through `schema_versions` to check that at least one active version
/// uses content that references the given subject/version.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn is_version_referenced(
    pool: &PgPool,
    subject: &str,
    version: i32,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r"SELECT EXISTS(
            SELECT 1 FROM schema_references sr
            JOIN schema_versions sv ON sr.content_id = sv.content_id
            WHERE sr.subject = $1 AND sr.version = $2
              AND sv.deleted = false
        )",
    )
    .bind(subject)
    .bind(version)
    .fetch_one(pool)
    .await
}
