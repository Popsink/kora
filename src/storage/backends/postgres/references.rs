//! Schema-reference operations for the `PostgreSQL` backend.

use crate::error::KoraError;
use crate::types::SchemaReference;

use super::{PgStorage, paged};

pub(super) async fn validate_references(
    store: &PgStorage,
    refs: &[SchemaReference],
) -> Result<(), KoraError> {
    for r in refs {
        let exists: bool = sqlx::query_scalar(
            r"SELECT EXISTS(
                SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                WHERE sub.name = $1 AND sv.version = $2
                  AND sv.deleted = false AND sub.deleted = false
            )",
        )
        .bind(&r.subject)
        .bind(r.version)
        .fetch_one(store.pool())
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

pub(super) async fn find_references_by_schema_id(
    store: &PgStorage,
    content_id: i64,
) -> Result<Vec<SchemaReference>, KoraError> {
    Ok(sqlx::query_as::<_, (String, String, i32)>(
        "SELECT name, subject, version FROM schema_references WHERE content_id = $1 ORDER BY name",
    )
    .bind(content_id)
    .fetch_all(store.pool())
    .await?
    .into_iter()
    .map(|(name, subject, version)| SchemaReference {
        name,
        subject,
        version,
    })
    .collect())
}

pub(super) async fn find_references_for_schema_ids(
    store: &PgStorage,
    content_ids: &[i64],
) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as::<_, (i64, String, String, i32)>(
        r"SELECT content_id, name, subject, version FROM schema_references
           WHERE content_id = ANY($1) ORDER BY content_id, name",
    )
    .bind(content_ids)
    .fetch_all(store.pool())
    .await?
    .into_iter()
    .map(|(content_id, name, subject, version)| {
        (
            content_id,
            SchemaReference {
                name,
                subject,
                version,
            },
        )
    })
    .collect())
}

pub(super) async fn find_referencing_schema_ids(
    store: &PgStorage,
    target_subject: &str,
    target_version: i32,
    include_deleted: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i64>, KoraError> {
    // SAFETY (sqlx 0.9 `SqlSafeStr`): pagination interpolates internal i64s
    // only; all values are bound.
    let sql = paged(
        r"SELECT DISTINCT sr.content_id
           FROM schema_references sr
           JOIN schema_versions sv ON sr.content_id = sv.content_id
           WHERE sr.subject = $1 AND sr.version = $2
             AND (sv.deleted = false OR $3)
           ORDER BY sr.content_id",
        offset,
        limit,
    );
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(target_subject)
        .bind(target_version)
        .bind(include_deleted)
        .fetch_all(store.pool())
        .await?)
}

pub(super) async fn is_version_referenced(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    Ok(sqlx::query_scalar(
        r"SELECT EXISTS(
            SELECT 1 FROM schema_references sr
            JOIN schema_versions sv ON sr.content_id = sv.content_id
            WHERE sr.subject = $1 AND sr.version = $2
              AND sv.deleted = false
        )",
    )
    .bind(subject)
    .bind(version)
    .fetch_one(store.pool())
    .await?)
}
