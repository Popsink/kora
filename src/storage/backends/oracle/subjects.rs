//! Oracle SQL for the subject-domain `Storage` operations.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::{fetch_strings, scalar_bool, scalar_opt_i64};
use crate::storage::types::HardDeleteResult;

use super::OracleStorage;
use super::driver::{cell_i32, cell_i64, like_pattern, query_all, s, val_i64};

pub(super) async fn list_subjects(
    store: &OracleStorage,
    include_deleted: bool,
    deleted_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    let filter = if deleted_only {
        "deleted = 1"
    } else if include_deleted {
        "1 = 1"
    } else {
        "deleted = 0"
    };
    if let Some(pat) = like_pattern(prefix) {
        let sql = format!(
            "SELECT name FROM subjects WHERE {filter} AND name LIKE :1 ESCAPE '\\' ORDER BY name"
        );
        fetch_strings(store, &sql, &binds![pat], offset, limit).await
    } else {
        let sql = format!("SELECT name FROM subjects WHERE {filter} ORDER BY name");
        fetch_strings(store, &sql, &[], offset, limit).await
    }
}

pub(super) async fn soft_delete_subject(
    store: &OracleStorage,
    name: &str,
) -> Result<Vec<i32>, KoraError> {
    let conn = store.conn().await?;
    let result = query_all(
        &conn,
        "SELECT sv.version FROM schema_versions sv \
         WHERE sv.subject_id = (SELECT id FROM subjects WHERE name = :1) AND sv.deleted = 0 \
         ORDER BY sv.version",
        &[s(name)],
        0,
        -1,
    )
    .await?;
    let mut versions = Vec::with_capacity(result.row_count());
    for row in result.iter() {
        versions.push(cell_i32(row, 0)?);
    }
    conn.execute_dml_sql(
        "UPDATE schema_versions SET deleted = 1 \
         WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) AND deleted = 0",
        &[s(name)],
    )
    .await?;
    conn.execute_dml_sql(
        "UPDATE subjects SET deleted = 1 WHERE name = :1 AND deleted = 0",
        &[s(name)],
    )
    .await?;
    conn.commit().await?;
    versions.sort_unstable();
    Ok(versions)
}

pub(super) async fn hard_delete_subject(
    store: &OracleStorage,
    name: &str,
) -> Result<HardDeleteResult, KoraError> {
    let conn = store.conn().await?;
    let found = conn
        .query(
            "SELECT id, deleted FROM subjects WHERE name = :1 FOR UPDATE",
            &[s(name)],
        )
        .await?;
    let Some(row) = found.first() else {
        return Ok(HardDeleteResult::NotFound);
    };
    let subject_id = cell_i64(row, 0)?;
    let deleted = cell_i64(row, 1)?;
    if deleted == 0 {
        conn.rollback().await?;
        return Ok(HardDeleteResult::NotSoftDeleted);
    }

    let vresult = query_all(
        &conn,
        &format!(
            "SELECT version FROM schema_versions \
             WHERE subject_id = {subject_id} AND deleted = 1 ORDER BY version"
        ),
        &[],
        0,
        -1,
    )
    .await?;
    let mut versions = Vec::with_capacity(vresult.row_count());
    for row in vresult.iter() {
        versions.push(cell_i32(row, 0)?);
    }

    for v in &versions {
        let referenced = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_references sr \
                        JOIN schema_versions sv ON sr.content_id = sv.content_id \
                        WHERE sr.subject = :1 AND sr.version = {v} AND sv.deleted = 0\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &[s(name)],
            )
            .await?;
        if referenced.first().and_then(|r| val_i64(r.get(0))) == Some(1) {
            conn.rollback().await?;
            return Ok(HardDeleteResult::ReferenceExists(format!(
                "{name} version {v}"
            )));
        }
    }

    conn.execute_dml_sql(
        &format!("DELETE FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 1"),
        &[],
    )
    .await?;

    let active = conn
        .query(
            &format!(
                "SELECT CASE WHEN EXISTS (\
                    SELECT 1 FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 0\
                 ) THEN 1 ELSE 0 END FROM dual"
            ),
            &[],
        )
        .await?;
    if active.first().and_then(|r| val_i64(r.get(0))) != Some(1) {
        conn.execute_dml_sql(
            &format!("DELETE FROM subjects WHERE id = {subject_id}"),
            &[],
        )
        .await?;
    }

    conn.commit().await?;
    versions.sort_unstable();
    Ok(HardDeleteResult::Deleted(versions))
}

pub(super) async fn find_subject_id_by_name(
    store: &OracleStorage,
    name: &str,
    include_deleted: bool,
) -> Result<Option<i64>, KoraError> {
    scalar_opt_i64(
        store,
        "SELECT id FROM subjects WHERE name = :1 AND (deleted = 0 OR :2 = 1)",
        &binds![name, include_deleted],
    )
    .await
}

pub(super) async fn subject_exists(
    store: &OracleStorage,
    name: &str,
    include_deleted: bool,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM subjects WHERE name = :1 AND (deleted = 0 OR :2 = 1)) \
         THEN 1 ELSE 0 END FROM dual",
        &binds![name, include_deleted],
    )
    .await
}

pub(super) async fn subject_is_soft_deleted(
    store: &OracleStorage,
    name: &str,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM subjects WHERE name = :1 AND deleted = 1) THEN 1 ELSE 0 END FROM dual",
        &binds![name],
    )
    .await
}
