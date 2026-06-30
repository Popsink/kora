//! Schema-reference operations for the `PostgreSQL` backend.
//!
//! All reads run through the SQL toolkit; the multi-id lookup inlines the
//! internal i64 ids into an IN-list because the toolkit's `Bind` cannot express
//! an array bind.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::scalar_bool;
use crate::storage::sql::{Row, SqlExecutor};
use crate::types::SchemaReference;

use super::PgStorage;

pub(super) async fn validate_references(
    store: &PgStorage,
    refs: &[SchemaReference],
) -> Result<(), KoraError> {
    for r in refs {
        let exists = scalar_bool(
            store,
            r"SELECT EXISTS(
                SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                WHERE sub.name = $1 AND sv.version = $2
                  AND sv.deleted = false AND sub.deleted = false
            )",
            &binds![&r.subject, r.version],
        )
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
    store
        .fetch_all(
            "SELECT name, subject, version FROM schema_references WHERE content_id = $1 ORDER BY name",
            &binds![content_id],
        )
        .await?
        .iter()
        .map(|r| {
            Ok(SchemaReference {
                name: r.get_str(0)?,
                subject: r.get_str(1)?,
                version: r.get_i32(2)?,
            })
        })
        .collect()
}

pub(super) async fn find_references_for_schema_ids(
    store: &PgStorage,
    content_ids: &[i64],
) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }
    // The toolkit's `Bind` cannot express an array bind (`= ANY($1)`), so the
    // internal i64 ids are inlined into an IN-list — one statement per call.
    let in_list = content_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT content_id, name, subject, version FROM schema_references \
         WHERE content_id IN ({in_list}) ORDER BY content_id, name"
    );
    store
        .fetch_all(&sql, &[])
        .await?
        .iter()
        .map(|r| {
            Ok((
                r.get_i64(0)?,
                SchemaReference {
                    name: r.get_str(1)?,
                    subject: r.get_str(2)?,
                    version: r.get_i32(3)?,
                },
            ))
        })
        .collect()
}

pub(super) async fn find_referencing_schema_ids(
    store: &PgStorage,
    target_subject: &str,
    target_version: i32,
    include_deleted: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i64>, KoraError> {
    store
        .fetch_all_paged(
            r"SELECT DISTINCT sr.content_id
               FROM schema_references sr
               JOIN schema_versions sv ON sr.content_id = sv.content_id
               WHERE sr.subject = $1 AND sr.version = $2
                 AND (sv.deleted = false OR $3)
               ORDER BY sr.content_id",
            &binds![target_subject, target_version, include_deleted],
            offset,
            limit,
        )
        .await?
        .iter()
        .map(|r| r.get_i64(0))
        .collect()
}

pub(super) async fn is_version_referenced(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        r"SELECT EXISTS(
            SELECT 1 FROM schema_references sr
            JOIN schema_versions sv ON sr.content_id = sv.content_id
            WHERE sr.subject = $1 AND sr.version = $2
              AND sv.deleted = false
        )",
        &binds![subject, version],
    )
    .await
}
