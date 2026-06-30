//! Oracle SQL for the schema-references-domain `Storage` operations.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::scalar_bool;
use crate::storage::sql::{Row as SqlRow, SqlExecutor};
use crate::types::SchemaReference;

use super::OracleStorage;
use super::driver::{cell_i32, cell_i64, i, query_all, s, val_i64};

pub(super) async fn validate_references(
    store: &OracleStorage,
    refs: &[SchemaReference],
) -> Result<(), KoraError> {
    let conn = store.conn().await?;
    for r in refs {
        let result = conn
            .query(
                "SELECT CASE WHEN EXISTS (\
                    SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                    WHERE sub.name = :1 AND sv.version = :2 \
                      AND sv.deleted = 0 AND sub.deleted = 0\
                 ) THEN 1 ELSE 0 END FROM dual",
                &[s(&r.subject), i(i64::from(r.version))],
            )
            .await?;
        if result.first().and_then(|row| val_i64(row.get(0))) != Some(1) {
            return Err(KoraError::ReferenceNotFound(format!(
                "Schema reference not found: subject '{}' version {}",
                r.subject, r.version
            )));
        }
    }
    Ok(())
}

pub(super) async fn find_references_by_schema_id(
    store: &OracleStorage,
    content_id: i64,
) -> Result<Vec<SchemaReference>, KoraError> {
    // Bind the id: this is called once per row of a listing, so inlining would
    // create a distinct statement per call and exhaust the session's cursors.
    store
        .fetch_all(
            "SELECT name, subject, version FROM schema_references \
             WHERE content_id = :1 ORDER BY name",
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
    store: &OracleStorage,
    content_ids: &[i64],
) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
    // Oracle caps an IN-list at 1000 expressions (ORA-01795), so chunk the
    // (internal i64) content ids. Inlining them keeps it one statement per
    // chunk — vs an N+1 of per-id queries — while staying under the cap.
    const ORACLE_IN_LIST_MAX: usize = 1000;
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn().await?;
    let mut out = Vec::new();
    for chunk in content_ids.chunks(ORACLE_IN_LIST_MAX) {
        let in_list = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let result = query_all(
            &conn,
            &format!(
                "SELECT content_id, name, subject, version FROM schema_references \
                 WHERE content_id IN ({in_list}) ORDER BY content_id, name"
            ),
            &[],
            0,
            -1,
        )
        .await?;
        for row in result.iter() {
            out.push((
                cell_i64(row, 0)?,
                SchemaReference {
                    name: row.get_string(1).unwrap_or_default().to_owned(),
                    subject: row.get_string(2).unwrap_or_default().to_owned(),
                    version: cell_i32(row, 3)?,
                },
            ));
        }
    }
    Ok(out)
}

pub(super) async fn find_referencing_schema_ids(
    store: &OracleStorage,
    target_subject: &str,
    target_version: i32,
    include_deleted: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i64>, KoraError> {
    store
        .fetch_all_paged(
            "SELECT DISTINCT sr.content_id FROM schema_references sr \
             JOIN schema_versions sv ON sr.content_id = sv.content_id \
             WHERE sr.subject = :1 AND sr.version = :2 \
               AND (sv.deleted = 0 OR :3 = 1) \
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
    store: &OracleStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_references sr \
            JOIN schema_versions sv ON sr.content_id = sv.content_id \
            WHERE sr.subject = :1 AND sr.version = :2 AND sv.deleted = 0\
         ) THEN 1 ELSE 0 END FROM dual",
        &binds![subject, version],
    )
    .await
}
