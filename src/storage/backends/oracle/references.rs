//! Oracle SQL for the schema-references-domain `Storage` operations.

use oracle::Connection;

use crate::error::KoraError;
use crate::types::SchemaReference;

use super::driver::{append_window, b, i, map_ora, s, scalar_bool, to_refs};

pub(super) fn validate_references(
    conn: &Connection,
    refs: &[SchemaReference],
) -> Result<(), KoraError> {
    for r in refs {
        let found = scalar_bool(
            conn,
            "SELECT CASE WHEN EXISTS (\
                SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                WHERE sub.name = :1 AND sv.version = :2 \
                  AND sv.deleted = 0 AND sub.deleted = 0\
             ) THEN 1 ELSE 0 END FROM dual",
            &[s(&r.subject), i(i64::from(r.version))],
        )?;
        if !found {
            return Err(KoraError::ReferenceNotFound(format!(
                "Schema reference not found: subject '{}' version {}",
                r.subject, r.version
            )));
        }
    }
    Ok(())
}

pub(super) fn find_references_by_schema_id(
    conn: &Connection,
    content_id: i64,
) -> Result<Vec<SchemaReference>, KoraError> {
    // Bind the id: this is called once per row of a listing, so inlining would
    // create a distinct statement per call.
    let binds = [i(content_id)];
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn
        .query(
            "SELECT name, subject, version FROM schema_references \
             WHERE content_id = :1 ORDER BY name",
            &refs,
        )
        .map_err(map_ora)?
    {
        let row = row.map_err(map_ora)?;
        out.push(SchemaReference {
            name: row.get::<usize, String>(0).map_err(map_ora)?,
            subject: row.get::<usize, String>(1).map_err(map_ora)?,
            version: row.get::<usize, i32>(2).map_err(map_ora)?,
        });
    }
    Ok(out)
}

pub(super) fn find_references_for_schema_ids(
    conn: &Connection,
    content_ids: &[i64],
) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
    // Oracle caps an IN-list at 1000 expressions (ORA-01795), so chunk the
    // (internal i64) content ids. Inlining them keeps it one statement per
    // chunk — vs an N+1 of per-id queries — while staying under the cap.
    const ORACLE_IN_LIST_MAX: usize = 1000;
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for chunk in content_ids.chunks(ORACLE_IN_LIST_MAX) {
        let in_list = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        for row in conn
            .query(
                &format!(
                    "SELECT content_id, name, subject, version FROM schema_references \
                     WHERE content_id IN ({in_list}) ORDER BY content_id, name"
                ),
                &[],
            )
            .map_err(map_ora)?
        {
            let row = row.map_err(map_ora)?;
            out.push((
                row.get::<usize, i64>(0).map_err(map_ora)?,
                SchemaReference {
                    name: row.get::<usize, String>(1).map_err(map_ora)?,
                    subject: row.get::<usize, String>(2).map_err(map_ora)?,
                    version: row.get::<usize, i32>(3).map_err(map_ora)?,
                },
            ));
        }
    }
    Ok(out)
}

pub(super) fn find_referencing_schema_ids(
    conn: &Connection,
    target_subject: &str,
    target_version: i32,
    include_deleted: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i64>, KoraError> {
    let base_sql = "SELECT DISTINCT sr.content_id FROM schema_references sr \
         JOIN schema_versions sv ON sr.content_id = sv.content_id \
         WHERE sr.subject = :1 AND sr.version = :2 \
           AND (sv.deleted = 0 OR :3 = 1) \
         ORDER BY sr.content_id";
    let sql = append_window(base_sql, offset, limit);
    let binds = [
        s(target_subject),
        i(i64::from(target_version)),
        b(include_deleted),
    ];
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        let row = row.map_err(map_ora)?;
        out.push(row.get::<usize, i64>(0).map_err(map_ora)?);
    }
    Ok(out)
}

pub(super) fn is_version_referenced(
    conn: &Connection,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS (\
            SELECT 1 FROM schema_references sr \
            JOIN schema_versions sv ON sr.content_id = sv.content_id \
            WHERE sr.subject = :1 AND sr.version = :2 AND sv.deleted = 0\
         ) THEN 1 ELSE 0 END FROM dual",
        &[s(subject), i(i64::from(version))],
    )
}
