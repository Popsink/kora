//! Oracle SQL for the schema-domain `Storage` operations, including the
//! transactional schema-registration path and the CLOB-aware finders/listings.

use oracle::{Connection, Row};

use crate::error::KoraError;
use crate::storage::types::{CompatCheck, NewSchema, SchemaVersion, SubjectVersion};
use crate::types::SchemaReference;

use super::driver::{
    OraBind, SV_COLS, SV_JOIN, append_window, b, clob, commit_or_rollback, first_row, i,
    is_unique_violation, like_pattern, map_ora, s, scalar_bool, scalar_opt, to_refs,
};

pub(super) fn register_schema_atomically(
    conn: &Connection,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[SchemaReference],
    normalize: bool,
    compat: Option<&CompatCheck>,
) -> Result<(i64, i32, bool), KoraError> {
    let r = register_once(conn, subject_name, schema, refs, normalize, compat);
    commit_or_rollback(conn, r)
}

pub(super) fn find_all_active_versions(
    conn: &Connection,
    subject: &str,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let binds = [s(subject)];
    collect_svs(
        conn,
        &format!(
            "SELECT {SV_COLS} {SV_JOIN} \
             WHERE sub.name = :1 AND sv.deleted = 0 ORDER BY sv.version"
        ),
        &binds,
    )
}

pub(super) fn find_schema_by_subject_version(
    conn: &Connection,
    subject: &str,
    version: i32,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let filter = if include_deleted {
        ""
    } else {
        " AND sv.deleted = 0"
    };
    let binds = [s(subject), i(i64::from(version))];
    let refs = to_refs(&binds);
    conn.query(
        &format!(
            "SELECT {SV_COLS} {SV_JOIN} \
             WHERE sub.name = :1 AND sv.version = :2{filter}"
        ),
        &refs,
    )
    .map_err(map_ora)?
    .next()
    .transpose()
    .map_err(map_ora)?
    .map(|row| row_to_sv(&row))
    .transpose()
}

pub(super) fn find_latest_schema_by_subject(
    conn: &Connection,
    subject: &str,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let filter = if include_deleted {
        ""
    } else {
        " AND sv.deleted = 0"
    };
    let binds = [s(subject)];
    let refs = to_refs(&binds);
    conn.query(
        &format!(
            "SELECT {SV_COLS} {SV_JOIN} \
             WHERE sub.name = :1{filter} ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY"
        ),
        &refs,
    )
    .map_err(map_ora)?
    .next()
    .transpose()
    .map_err(map_ora)?
    .map(|row| row_to_sv(&row))
    .transpose()
}

pub(super) fn find_schema_by_subject_id_and_fingerprint(
    conn: &Connection,
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
    let binds = [i(subject_id), s(fingerprint)];
    let refs = to_refs(&binds);
    conn.query(
        &format!(
            "SELECT {SV_COLS} {SV_JOIN} \
             WHERE sv.subject_id = :1 AND sc.{fp_col} = :2{filter}"
        ),
        &refs,
    )
    .map_err(map_ora)?
    .next()
    .transpose()
    .map_err(map_ora)?
    .map(|row| row_to_sv(&row))
    .transpose()
}

pub(super) fn find_schema_by_id(
    conn: &Connection,
    id: i64,
) -> Result<Option<(String, String)>, KoraError> {
    let binds = [i(id)];
    let refs = to_refs(&binds);
    conn.query(
        "SELECT schema_text, schema_type FROM schema_contents WHERE id = :1",
        &refs,
    )
    .map_err(map_ora)?
    .next()
    .transpose()
    .map_err(map_ora)?
    .map(|row| -> Result<(String, String), KoraError> {
        let text = row.get::<usize, String>(0).map_err(map_ora)?;
        let kind = row.get::<usize, String>(1).map_err(map_ora)?;
        Ok((text, kind))
    })
    .transpose()
}

pub(super) fn find_max_schema_id(conn: &Connection) -> Result<i64, KoraError> {
    // `MAX(id)` always returns a row, but its value is NULL on an empty table.
    Ok(
        scalar_opt::<Option<i64>>(conn, "SELECT MAX(id) FROM schema_contents", &[])?
            .flatten()
            .unwrap_or(0),
    )
}

pub(super) fn schema_exists(conn: &Connection, id: i64) -> Result<bool, KoraError> {
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM schema_contents WHERE id = :1) THEN 1 ELSE 0 END FROM dual",
        &[i(id)],
    )
}

pub(super) fn find_subjects_by_schema_id(
    conn: &Connection,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    // The `include_deleted` flag appears twice (`:2` and `:3`); each placeholder
    // consumes one param, in SQL appearance order.
    let (base_sql, binds): (&str, Vec<OraBind>) = if let Some(filter) = subject_filter {
        (
            "SELECT DISTINCT sub.name FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) AND sub.name = :4 \
             ORDER BY sub.name",
            vec![i(id), b(include_deleted), b(include_deleted), s(filter)],
        )
    } else {
        (
            "SELECT DISTINCT sub.name FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) \
             ORDER BY sub.name",
            vec![i(id), b(include_deleted), b(include_deleted)],
        )
    };
    let sql = append_window(base_sql, offset, limit);
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        let row = row.map_err(map_ora)?;
        out.push(row.get::<usize, String>(0).map_err(map_ora)?);
    }
    Ok(out)
}

pub(super) fn find_versions_by_schema_id(
    conn: &Connection,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SubjectVersion>, KoraError> {
    // `include_deleted` appears twice (`:2`, `:3`) — one param per occurrence.
    let (base_sql, binds): (&str, Vec<OraBind>) = if let Some(filter) = subject_filter {
        (
            "SELECT sub.name, sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) AND sub.name = :4 \
             ORDER BY sub.name, sv.version",
            vec![i(id), b(include_deleted), b(include_deleted), s(filter)],
        )
    } else {
        (
            "SELECT sub.name, sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = :1 AND (sv.deleted = 0 OR :2 = 1) \
               AND (sub.deleted = 0 OR :3 = 1) \
             ORDER BY sub.name, sv.version",
            vec![i(id), b(include_deleted), b(include_deleted)],
        )
    };
    let sql = append_window(base_sql, offset, limit);
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        let row = row.map_err(map_ora)?;
        out.push(SubjectVersion {
            subject: row.get::<usize, String>(0).map_err(map_ora)?,
            version: row.get::<usize, i32>(1).map_err(map_ora)?,
        });
    }
    Ok(out)
}

pub(super) fn list_schema_versions(
    conn: &Connection,
    subject: &str,
    include_deleted: bool,
    deleted_only: bool,
    deleted_as_negative: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i32>, KoraError> {
    let (base_sql, binds): (&str, Vec<OraBind>) = if deleted_only && deleted_as_negative {
        (
            "SELECT -sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version",
            vec![s(subject)],
        )
    } else if deleted_only {
        (
            "SELECT sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version",
            vec![s(subject)],
        )
    } else if deleted_as_negative && include_deleted {
        (
            "SELECT CASE WHEN sv.deleted = 1 THEN -sv.version ELSE sv.version END \
             FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 ORDER BY ABS(sv.version)",
            vec![s(subject)],
        )
    } else {
        (
            "SELECT sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sub.name = :1 AND (sv.deleted = 0 OR :2 = 1) \
             ORDER BY sv.version",
            vec![s(subject), b(include_deleted)],
        )
    };
    let sql = append_window(base_sql, offset, limit);
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        let row = row.map_err(map_ora)?;
        out.push(row.get::<usize, i32>(0).map_err(map_ora)?);
    }
    Ok(out)
}

pub(super) fn list_schemas(
    conn: &Connection,
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
    let base_sql = if latest_only {
        // DISTINCT ON (sub.name) → highest version per subject via ROW_NUMBER.
        format!(
            "SELECT id, subject, version, schema_type, schema_text FROM (\
                SELECT sc.id AS id, sub.name AS subject, sv.version AS version, \
                       sc.schema_type AS schema_type, sc.schema_text AS schema_text, \
                       ROW_NUMBER() OVER (PARTITION BY sub.name ORDER BY sv.version DESC) AS rn \
                {SV_JOIN} \
                WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql}\
             ) WHERE rn = 1 ORDER BY subject"
        )
    } else {
        format!(
            "SELECT {SV_COLS} {SV_JOIN} \
             WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql} \
             ORDER BY sub.name, sv.version"
        )
    };
    let sql = append_window(&base_sql, offset, limit);
    let binds: Vec<OraBind> = like.iter().map(|p| s(p)).collect();
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        out.push(row_to_sv(&row.map_err(map_ora)?)?);
    }
    Ok(out)
}

pub(super) fn soft_delete_latest_schema(
    conn: &Connection,
    subject: &str,
) -> Result<Option<i32>, KoraError> {
    let r = (|| -> Result<Option<i32>, KoraError> {
        let binds = [s(subject)];
        let refs = to_refs(&binds);
        let Some(version) = conn
            .query(
                "SELECT sv.version FROM schema_versions sv \
                 JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 AND sv.deleted = 0 \
                 ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY",
                &refs,
            )
            .map_err(map_ora)?
            .next()
            .transpose()
            .map_err(map_ora)?
            .map(|r| r.get::<usize, i32>(0))
            .transpose()
            .map_err(map_ora)?
        else {
            return Ok(None);
        };
        let updated = conn
            .execute(
                &format!(
                    "UPDATE schema_versions SET deleted = 1 \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 0"
                ),
                &refs,
            )
            .map_err(map_ora)?
            .row_count()
            .map_err(map_ora)?;
        Ok((updated >= 1).then_some(version))
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn soft_delete_schema_version(
    conn: &Connection,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    let r = (|| -> Result<Option<i32>, KoraError> {
        let binds = [s(subject)];
        let refs = to_refs(&binds);
        let updated = conn
            .execute(
                &format!(
                    "UPDATE schema_versions SET deleted = 1 \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 0"
                ),
                &refs,
            )
            .map_err(map_ora)?
            .row_count()
            .map_err(map_ora)?;
        Ok((updated >= 1).then_some(version))
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn hard_delete_schema_version(
    conn: &Connection,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    let r = (|| -> Result<Option<i32>, KoraError> {
        let binds = [s(subject)];
        let refs = to_refs(&binds);
        let deleted = conn
            .execute(
                &format!(
                    "DELETE FROM schema_versions \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 1"
                ),
                &refs,
            )
            .map_err(map_ora)?
            .row_count()
            .map_err(map_ora)?;
        Ok((deleted >= 1).then_some(version))
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn version_is_soft_deleted(
    conn: &Connection,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    let binds = [s(subject), i(i64::from(version))];
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
            WHERE sub.name = :1 AND sv.version = :2 AND sv.deleted = 1\
         ) THEN 1 ELSE 0 END FROM dual",
        &binds,
    )
}

pub(super) fn version_is_active(
    conn: &Connection,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    let binds = [s(subject), i(i64::from(version))];
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
            WHERE sub.name = :1 AND sv.version = :2 AND sv.deleted = 0\
         ) THEN 1 ELSE 0 END FROM dual",
        &binds,
    )
}

// -- Registration helpers --

/// Perform one registration in a single transaction (the caller commits/rolls
/// back). Idempotent: when the fingerprint already maps to an active version it
/// returns that `(content_id, version, false)`; otherwise it inserts a new
/// version and returns `(content_id, version, true)`.
fn register_once(
    conn: &Connection,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[SchemaReference],
    normalize: bool,
    compat: Option<&CompatCheck>,
) -> Result<(i64, i32, bool), KoraError> {
    let subject_id = upsert_subject(conn, subject_name)?;

    // Per-subject idempotency: existing active version with this fingerprint?
    let (fp, fp_col) = if normalize {
        (schema.fingerprint, "fingerprint")
    } else {
        (schema.raw_fingerprint, "raw_fingerprint")
    };
    let existing = first_row(
        conn,
        &format!(
            "SELECT sv.content_id, sv.version FROM schema_versions sv \
             JOIN schema_contents sc ON sv.content_id = sc.id \
             WHERE sv.subject_id = {subject_id} AND sc.{fp_col} = :1 AND sv.deleted = 0 \
             ORDER BY sv.version FETCH FIRST 1 ROW ONLY"
        ),
        &[s(fp)],
    )?;
    if let Some(row) = existing {
        let content_id = row.get::<usize, i64>(0).map_err(map_ora)?;
        let version = row.get::<usize, i32>(1).map_err(map_ora)?;
        return Ok((content_id, version, false));
    }

    // Compatibility check inside the transaction (after the subject lock).
    if let Some(compat) = compat {
        run_compat_check(conn, subject_id, compat)?;
    }

    let content_id = upsert_content(conn, schema)?;

    // Next version under the locked subject, then insert.
    let version = scalar_opt::<i64>(
        conn,
        &format!(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM schema_versions WHERE subject_id = {subject_id}"
        ),
        &[],
    )?
    .and_then(|v| i32::try_from(v).ok())
    .ok_or_else(|| KoraError::BackendDataStore("could not compute next version".to_owned()))?;
    conn.execute(
        &format!(
            "INSERT INTO schema_versions (subject_id, version, content_id) \
             VALUES ({subject_id}, {version}, {content_id})"
        ),
        &[],
    )
    .map_err(map_ora)?;

    // Store references only when provided and the content has none yet.
    if !refs.is_empty() {
        let has_refs = scalar_bool(
            conn,
            &format!(
                "SELECT CASE WHEN EXISTS \
                 (SELECT 1 FROM schema_references WHERE content_id = {content_id}) \
                 THEN 1 ELSE 0 END FROM dual"
            ),
            &[],
        )?;
        if !has_refs {
            for r in refs {
                let ref_binds = [s(&r.name), s(&r.subject)];
                let ref_refs = to_refs(&ref_binds);
                conn.execute(
                    &format!(
                        "INSERT INTO schema_references (content_id, name, subject, version) \
                         VALUES ({content_id}, :1, :2, {})",
                        r.version
                    ),
                    &ref_refs,
                )
                .map_err(map_ora)?;
            }
        }
    }

    Ok((content_id, version, true))
}

/// Upsert a subject by name and return its id, holding a row lock on it.
fn upsert_subject(conn: &Connection, name: &str) -> Result<i64, KoraError> {
    let binds = [s(name)];
    let refs = to_refs(&binds);
    let updated = conn
        .execute(
            "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
            &refs,
        )
        .map_err(map_ora)?
        .row_count()
        .map_err(map_ora)?;
    if updated == 0 {
        // Insert; a concurrent insert (ORA-00001) means the row now exists, so
        // re-run the update to re-activate and lock it.
        match conn.execute("INSERT INTO subjects (name) VALUES (:1)", &refs) {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                conn.execute(
                    "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
                    &refs,
                )
                .map_err(map_ora)?;
            }
            Err(e) => return Err(map_ora(e)),
        }
    }
    scalar_opt::<i64>(conn, "SELECT id FROM subjects WHERE name = :1", &binds)?
        .ok_or_else(|| KoraError::BackendDataStore("subject id missing after upsert".to_owned()))
}

/// Deduplicate schema content globally and return its id (`ON CONFLICT
/// (raw_fingerprint)` equivalent).
fn upsert_content(conn: &Connection, schema: &NewSchema<'_>) -> Result<i64, KoraError> {
    let raw_fp = schema.raw_fingerprint;
    let fp_binds = [s(raw_fp)];
    let select_id = "SELECT id FROM schema_contents WHERE raw_fingerprint = :1";
    if let Some(id) = scalar_opt::<i64>(conn, select_id, &fp_binds)? {
        // Mirror EXCLUDED.schema_type from the Postgres upsert.
        let upd_binds = [s(schema.schema_type)];
        let upd_refs = to_refs(&upd_binds);
        conn.execute(
            &format!("UPDATE schema_contents SET schema_type = :1 WHERE id = {id}"),
            &upd_refs,
        )
        .map_err(map_ora)?;
        return Ok(id);
    }

    // `schema_text` / `canonical_form` are CLOB columns; bind them explicitly as
    // CLOB so values larger than ~32 KB write correctly (a plain string bind
    // would default to NVARCHAR2 and fail with ORA-01461 / ORA-22835).
    let ins_binds = [
        s(schema.schema_type),
        clob(schema.schema_text),
        clob(schema.canonical_form),
        s(schema.fingerprint),
        s(raw_fp),
    ];
    let ins_refs = to_refs(&ins_binds);
    match conn.execute(
        "INSERT INTO schema_contents \
            (schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint) \
         VALUES (:1, :2, :3, :4, :5)",
        &ins_refs,
    ) {
        Ok(_) => {}
        Err(e) if is_unique_violation(&e) => {} // concurrent insert — re-select below
        Err(e) => return Err(map_ora(e)),
    }
    scalar_opt::<i64>(conn, select_id, &fp_binds)?
        .ok_or_else(|| KoraError::BackendDataStore("content id missing after upsert".to_owned()))
}

/// Run the in-transaction compatibility check (mirror of the Postgres path).
fn run_compat_check(
    conn: &Connection,
    subject_id: i64,
    compat: &CompatCheck,
) -> Result<(), KoraError> {
    // Transitive mode (empty `versions`) re-fetches all versions inside the
    // transaction; non-transitive uses the pre-fetched set. Evaluation is shared.
    if compat.versions.is_empty() {
        let versions = collect_svs(
            conn,
            &format!(
                "SELECT {SV_COLS} {SV_JOIN} \
                 WHERE sv.subject_id = {subject_id} AND sv.deleted = 0 ORDER BY sv.version"
            ),
            &[],
        )?;
        crate::storage::compat::evaluate(&versions, compat)
    } else {
        crate::storage::compat::evaluate(&compat.versions, compat)
    }
}

// -- Local helpers --

/// Map a row selecting [`SV_COLS`] (in order: id, subject, version,
/// `schema_type`, `schema_text`) to a [`SchemaVersion`]. `schema_text` is a CLOB
/// read inline.
fn row_to_sv(row: &Row) -> Result<SchemaVersion, KoraError> {
    Ok(SchemaVersion {
        id: row.get::<usize, i64>(0).map_err(map_ora)?,
        subject: row.get::<usize, String>(1).map_err(map_ora)?,
        version: row.get::<usize, i32>(2).map_err(map_ora)?,
        schema_type: row.get::<usize, String>(3).map_err(map_ora)?,
        schema: row.get::<usize, String>(4).map_err(map_ora)?,
        references: Vec::new(),
    })
}

/// Run a SELECT of [`SV_COLS`] and collect the rows as [`SchemaVersion`]s.
fn collect_svs(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<Vec<SchemaVersion>, KoraError> {
    let refs = to_refs(binds);
    let mut out = Vec::new();
    for row in conn.query(sql, &refs).map_err(map_ora)? {
        out.push(row_to_sv(&row.map_err(map_ora)?)?);
    }
    Ok(out)
}
