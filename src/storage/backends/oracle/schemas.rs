//! Oracle SQL for the schema-domain `Storage` operations, including the
//! transactional schema-registration path and the CLOB-aware finders/listings.

use oracle_rs::Connection;

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::scalar_bool;
use crate::storage::sql::helpers::scalar_opt_i64;
use crate::storage::sql::{Row as SqlRow, SqlExecutor};
use crate::storage::types::{CompatCheck, NewSchema, SchemaVersion, SubjectVersion};
use crate::types::SchemaReference;

use super::OracleStorage;
use super::driver::{
    SV_COLS, SV_COLS_META, SV_JOIN, cell_i32, cell_i64, collect_svs, is_unique_violation,
    like_pattern, query_all, row_to_sv, s, val_i64,
};

pub(super) async fn register_schema_atomically(
    store: &OracleStorage,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[SchemaReference],
    normalize: bool,
    compat: Option<CompatCheck>,
) -> Result<(i64, i32, bool), KoraError> {
    // `oracle-rs` binds CLOB values (schema_text / canonical_form) through a
    // temporary LOB, which Oracle intermittently rejects under concurrency
    // ("server rejected … temporary LOB"), closing the connection. Registration
    // is idempotent (content/version dedup), so retry the whole operation on a
    // fresh connection for that transient class of error.
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let conn = store.conn().await?;
        match register_once(&conn, subject_name, schema, refs, normalize, compat.clone()).await {
            Ok(v) => return Ok(v),
            // Transient connection-level failure: loop and retry on a fresh connection.
            Err(e) if attempt < 4 && is_transient(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

pub(super) async fn find_all_active_versions(
    store: &OracleStorage,
    subject: &str,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let conn = store.conn().await?;
    let result = query_all(
        &conn,
        &format!(
            "SELECT {SV_COLS_META} {SV_JOIN} \
             WHERE sub.name = :1 AND sv.deleted = 0 ORDER BY sv.version"
        ),
        &[s(subject)],
        0,
        -1,
    )
    .await?;
    collect_svs(&conn, &result).await
}

pub(super) async fn find_schema_by_subject_version(
    store: &OracleStorage,
    subject: &str,
    version: i32,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let filter = if include_deleted {
        ""
    } else {
        " AND sv.deleted = 0"
    };
    let conn = store.conn().await?;
    let result = conn
        .query(
            &format!(
                "SELECT {SV_COLS} {SV_JOIN} \
                 WHERE sub.name = :1 AND sv.version = {version}{filter}"
            ),
            &[s(subject)],
        )
        .await?;
    match result.first() {
        Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
        None => Ok(None),
    }
}

pub(super) async fn find_latest_schema_by_subject(
    store: &OracleStorage,
    subject: &str,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let filter = if include_deleted {
        ""
    } else {
        " AND sv.deleted = 0"
    };
    let conn = store.conn().await?;
    let result = conn
        .query(
            &format!(
                "SELECT {SV_COLS} {SV_JOIN} \
                 WHERE sub.name = :1{filter} ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY"
            ),
            &[s(subject)],
        )
        .await?;
    match result.first() {
        Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
        None => Ok(None),
    }
}

pub(super) async fn find_schema_by_subject_id_and_fingerprint(
    store: &OracleStorage,
    subject_id: i64,
    fingerprint: &str,
    normalize: bool,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let fp_col = if normalize {
        "fingerprint"
    } else {
        "raw_fingerprint"
    };
    let filter = if include_deleted {
        ""
    } else {
        " AND sv.deleted = 0"
    };
    let conn = store.conn().await?;
    let result = conn
        .query(
            &format!(
                "SELECT {SV_COLS} {SV_JOIN} \
                 WHERE sv.subject_id = {subject_id} AND sc.{fp_col} = :1{filter}"
            ),
            &[s(fingerprint)],
        )
        .await?;
    match result.first() {
        Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
        None => Ok(None),
    }
}

pub(super) async fn find_schema_by_id(
    store: &OracleStorage,
    id: i64,
) -> Result<Option<(String, String)>, KoraError> {
    let conn = store.conn().await?;
    let result = conn
        .query(
            &format!("SELECT schema_text, schema_type FROM schema_contents WHERE id = {id}"),
            &[],
        )
        .await?;
    match result.first() {
        Some(row) => {
            let text = super::driver::cell_text(&conn, row.get(0)).await?;
            let kind = super::driver::cell_text(&conn, row.get(1)).await?;
            Ok(Some((text, kind)))
        }
        None => Ok(None),
    }
}

pub(super) async fn find_max_schema_id(store: &OracleStorage) -> Result<i64, KoraError> {
    Ok(
        scalar_opt_i64(store, "SELECT MAX(id) FROM schema_contents", &[])
            .await?
            .unwrap_or(0),
    )
}

pub(super) async fn schema_exists(store: &OracleStorage, id: i64) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM schema_contents WHERE id = :1) THEN 1 ELSE 0 END FROM dual",
        &binds![id],
    )
    .await
}

pub(super) async fn find_subjects_by_schema_id(
    store: &OracleStorage,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    // The driver binds `:N` positionally by occurrence, so each placeholder
    // consumes one param; the `include_deleted` flag appears twice and is
    // therefore passed twice (`:2` and `:3`), in SQL appearance order.
    if let Some(filter) = subject_filter {
        let sql = "SELECT DISTINCT sub.name FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) AND sub.name = :4 \
             ORDER BY sub.name";
        crate::storage::sql::helpers::fetch_strings(
            store,
            sql,
            &binds![id, include_deleted, include_deleted, filter],
            offset,
            limit,
        )
        .await
    } else {
        let sql = "SELECT DISTINCT sub.name FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) \
             ORDER BY sub.name";
        crate::storage::sql::helpers::fetch_strings(
            store,
            sql,
            &binds![id, include_deleted, include_deleted],
            offset,
            limit,
        )
        .await
    }
}

pub(super) async fn find_versions_by_schema_id(
    store: &OracleStorage,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SubjectVersion>, KoraError> {
    // `include_deleted` appears twice (`:2`, `:3`) — one param per occurrence.
    let rows = if let Some(filter) = subject_filter {
        let sql = "SELECT sub.name, sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) AND sub.name = :4 \
             ORDER BY sub.name, sv.version";
        store
            .fetch_all_paged(
                sql,
                &binds![id, include_deleted, include_deleted, filter],
                offset,
                limit,
            )
            .await?
    } else {
        let sql = "SELECT sub.name, sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) \
             ORDER BY sub.name, sv.version";
        store
            .fetch_all_paged(
                sql,
                &binds![id, include_deleted, include_deleted],
                offset,
                limit,
            )
            .await?
    };
    rows.iter()
        .map(|r| {
            Ok(SubjectVersion {
                subject: r.get_str(0)?,
                version: r.get_i32(1)?,
            })
        })
        .collect()
}

pub(super) async fn list_schema_versions(
    store: &OracleStorage,
    subject: &str,
    include_deleted: bool,
    deleted_only: bool,
    deleted_as_negative: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i32>, KoraError> {
    let rows = if deleted_only && deleted_as_negative {
        let sql = "SELECT -sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else if deleted_only {
        let sql = "SELECT sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else if deleted_as_negative && include_deleted {
        let sql = "SELECT CASE WHEN sv.deleted = 1 THEN -sv.version ELSE sv.version END \
             FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 ORDER BY ABS(sv.version)";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else {
        let sql = "SELECT sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND (sv.deleted = 0 OR :2 = 1) \
             ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject, include_deleted], offset, limit)
            .await?
    };
    rows.iter().map(|r| r.get_i32(0)).collect()
}

pub(super) async fn list_schemas(
    store: &OracleStorage,
    include_deleted: bool,
    latest_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let like = like_pattern(prefix);
    let incl = i64::from(include_deleted);
    let like_sql = if like.is_some() {
        " AND sub.name LIKE :1 ESCAPE '\\'"
    } else {
        ""
    };
    let sql = if latest_only {
        // DISTINCT ON (sub.name) → highest version per subject via ROW_NUMBER.
        format!(
            "SELECT id, subject, version, schema_type FROM (\
                SELECT sc.id AS id, sub.name AS subject, sv.version AS version, \
                       sc.schema_type AS schema_type, \
                       ROW_NUMBER() OVER (PARTITION BY sub.name ORDER BY sv.version DESC) AS rn \
                {SV_JOIN} \
                WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql}\
             ) WHERE rn = 1 ORDER BY subject"
        )
    } else {
        format!(
            "SELECT {SV_COLS_META} {SV_JOIN} \
             WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql} \
             ORDER BY sub.name, sv.version"
        )
    };
    let params: Vec<oracle_rs::Value> = like.iter().map(|p| s(p)).collect();
    let conn = store.conn().await?;
    let result = query_all(&conn, &sql, &params, offset, limit).await?;
    collect_svs(&conn, &result).await
}

pub(super) async fn soft_delete_latest_schema(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<i32>, KoraError> {
    let conn = store.conn().await?;
    let latest = conn
        .query(
            "SELECT sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND sv.deleted = 0 \
             ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY",
            &[s(subject)],
        )
        .await?;
    let Some(version) = latest.first().map(|r| cell_i32(r, 0)).transpose()? else {
        return Ok(None);
    };
    let updated = conn
        .execute_dml_sql(
            &format!(
                "UPDATE schema_versions SET deleted = 1 \
                 WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                   AND version = {version} AND deleted = 0"
            ),
            &[s(subject)],
        )
        .await?;
    conn.commit().await?;
    Ok((updated >= 1).then_some(version))
}

pub(super) async fn soft_delete_schema_version(
    store: &OracleStorage,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    let conn = store.conn().await?;
    let updated = conn
        .execute_dml_sql(
            &format!(
                "UPDATE schema_versions SET deleted = 1 \
                 WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                   AND version = {version} AND deleted = 0"
            ),
            &[s(subject)],
        )
        .await?;
    conn.commit().await?;
    Ok((updated >= 1).then_some(version))
}

pub(super) async fn hard_delete_schema_version(
    store: &OracleStorage,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    let conn = store.conn().await?;
    let deleted = conn
        .execute_dml_sql(
            &format!(
                "DELETE FROM schema_versions \
                 WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                   AND version = {version} AND deleted = 1"
            ),
            &[s(subject)],
        )
        .await?;
    conn.commit().await?;
    Ok((deleted >= 1).then_some(version))
}

pub(super) async fn version_is_soft_deleted(
    store: &OracleStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
            WHERE sub.name = :1 AND sv.version = :2 AND sv.deleted = 1\
         ) THEN 1 ELSE 0 END FROM dual",
        &binds![subject, version],
    )
    .await
}

pub(super) async fn version_is_active(
    store: &OracleStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
            WHERE sub.name = :1 AND sv.version = :2 AND sv.deleted = 0\
         ) THEN 1 ELSE 0 END FROM dual",
        &binds![subject, version],
    )
    .await
}

// -- Registration helpers --

/// Perform one registration attempt against `conn` in a single transaction.
///
/// Idempotent: when the fingerprint already maps to an active version it
/// returns that `(content_id, version, false)`; otherwise it inserts a new
/// version and returns `(content_id, version, true)`. Because it is idempotent,
/// `register_schema_atomically` can safely retry it on a fresh connection after
/// a transient connection-level failure (see `is_transient`).
async fn register_once(
    conn: &Connection,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[SchemaReference],
    normalize: bool,
    compat: Option<CompatCheck>,
) -> Result<(i64, i32, bool), KoraError> {
    let subject_id = upsert_subject(conn, subject_name).await?;

    // Per-subject idempotency: existing active version with this fingerprint?
    let (fp, fp_col) = if normalize {
        (schema.fingerprint, "fingerprint")
    } else {
        (schema.raw_fingerprint, "raw_fingerprint")
    };
    let existing = conn
        .query(
            &format!(
                "SELECT sv.content_id, sv.version FROM schema_versions sv \
                 JOIN schema_contents sc ON sv.content_id = sc.id \
                 WHERE sv.subject_id = {subject_id} AND sc.{fp_col} = :1 AND sv.deleted = 0 \
                 ORDER BY sv.version FETCH FIRST 1 ROW ONLY"
            ),
            &[s(fp)],
        )
        .await?;
    if let Some(row) = existing.first() {
        let content_id = cell_i64(row, 0)?;
        let version = cell_i32(row, 1)?;
        conn.commit().await?;
        return Ok((content_id, version, false));
    }

    // Compatibility check inside the transaction (after the subject lock).
    if let Some(compat) = compat {
        run_compat_check(conn, subject_id, compat).await?;
    }

    let content_id = upsert_content(conn, schema).await?;

    // Next version under the locked subject, then insert.
    let next = conn
        .query(
            &format!(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM schema_versions WHERE subject_id = {subject_id}"
            ),
            &[],
        )
        .await?;
    let version = next
        .first()
        .and_then(|r| val_i64(r.get(0)))
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| KoraError::BackendDataStore("could not compute next version".to_owned()))?;
    conn.execute_dml_sql(
        &format!(
            "INSERT INTO schema_versions (subject_id, version, content_id) \
             VALUES ({subject_id}, {version}, {content_id})"
        ),
        &[],
    )
    .await?;

    // Store references only when provided and the content has none yet.
    if !refs.is_empty() {
        let has = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS \
                     (SELECT 1 FROM schema_references WHERE content_id = {content_id}) \
                     THEN 1 ELSE 0 END FROM dual"
                ),
                &[],
            )
            .await?;
        if has.first().and_then(|r| val_i64(r.get(0))) != Some(1) {
            for r in refs {
                conn.execute_dml_sql(
                    &format!(
                        "INSERT INTO schema_references (content_id, name, subject, version) \
                         VALUES ({content_id}, :1, :2, {})",
                        r.version
                    ),
                    &[s(&r.name), s(&r.subject)],
                )
                .await?;
            }
        }
    }

    conn.commit().await?;
    Ok((content_id, version, true))
}

/// Whether an error is a transient, connection-level failure worth retrying on
/// a fresh connection.
///
/// `oracle-rs` occasionally binds CLOB values through a temporary LOB that
/// Oracle rejects under concurrency, severing the connection (surfaced as
/// `ORA-00000` / "temporary LOB" / "closed the connection", and the follow-on
/// `ORA-03113`/`ORA-03114`). Registration is idempotent, so these are safe to
/// retry. Schema-level errors (incompatibility, missing references) are NOT
/// transient and must surface immediately.
fn is_transient(e: &KoraError) -> bool {
    let KoraError::BackendDataStore(msg) = e else {
        return false;
    };
    let m = msg.to_ascii_lowercase();
    m.contains("closed the connection")
        || m.contains("temporary lob")
        || m.contains("ora-00000")
        || m.contains("ora-03113")
        || m.contains("ora-03114")
}

/// Upsert a subject by name and return its id, holding a row lock on it.
async fn upsert_subject(conn: &Connection, name: &str) -> Result<i64, KoraError> {
    let updated = conn
        .execute_dml_sql(
            "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
            &[s(name)],
        )
        .await?;
    if updated == 0 {
        // Insert; a concurrent insert (ORA-00001) means the row now exists, so
        // re-run the update to re-activate and lock it.
        match conn
            .execute_dml_sql("INSERT INTO subjects (name) VALUES (:1)", &[s(name)])
            .await
        {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                conn.execute_dml_sql(
                    "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
                    &[s(name)],
                )
                .await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let result = conn
        .query("SELECT id FROM subjects WHERE name = :1", &[s(name)])
        .await?;
    result
        .first()
        .and_then(|row| val_i64(row.get(0)))
        .ok_or_else(|| KoraError::BackendDataStore("subject id missing after upsert".to_owned()))
}

/// Deduplicate schema content globally and return its id (`ON CONFLICT
/// (raw_fingerprint)` equivalent).
async fn upsert_content(conn: &Connection, schema: &NewSchema<'_>) -> Result<i64, KoraError> {
    let raw_fp = schema.raw_fingerprint;
    let existing = conn
        .query(
            "SELECT id FROM schema_contents WHERE raw_fingerprint = :1",
            &[s(raw_fp)],
        )
        .await?;
    if let Some(id) = existing.first().and_then(|row| val_i64(row.get(0))) {
        // Mirror EXCLUDED.schema_type from the Postgres upsert.
        conn.execute_dml_sql(
            &format!("UPDATE schema_contents SET schema_type = :1 WHERE id = {id}"),
            &[s(schema.schema_type)],
        )
        .await?;
        return Ok(id);
    }

    let insert = conn
        .execute_dml_sql(
            "INSERT INTO schema_contents \
                (schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint) \
             VALUES (:1, :2, :3, :4, :5)",
            &[
                s(schema.schema_type),
                s(schema.schema_text),
                s(schema.canonical_form),
                s(schema.fingerprint),
                s(raw_fp),
            ],
        )
        .await;
    match insert {
        Ok(_) => {}
        Err(e) if is_unique_violation(&e) => {} // concurrent insert — re-select below
        Err(e) => return Err(e.into()),
    }
    let result = conn
        .query(
            "SELECT id FROM schema_contents WHERE raw_fingerprint = :1",
            &[s(raw_fp)],
        )
        .await?;
    result
        .first()
        .and_then(|row| val_i64(row.get(0)))
        .ok_or_else(|| KoraError::BackendDataStore("content id missing after upsert".to_owned()))
}

/// Run the in-transaction compatibility check (mirror of the Postgres path).
async fn run_compat_check(
    conn: &Connection,
    subject_id: i64,
    compat: CompatCheck,
) -> Result<(), KoraError> {
    // Transitive mode (empty `versions`) re-fetches all versions inside the
    // transaction; non-transitive uses the pre-fetched set. Evaluation is shared.
    if compat.versions.is_empty() {
        let result = query_all(
            conn,
            &format!(
                "SELECT {SV_COLS_META} {SV_JOIN} \
                 WHERE sv.subject_id = {subject_id} AND sv.deleted = 0 ORDER BY sv.version"
            ),
            &[],
            0,
            -1,
        )
        .await?;
        let versions = collect_svs(conn, &result).await?;
        crate::storage::compat::evaluate(&versions, &compat)
    } else {
        crate::storage::compat::evaluate(&compat.versions, &compat)
    }
}
