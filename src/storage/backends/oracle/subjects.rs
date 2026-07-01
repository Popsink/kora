//! Oracle SQL for the subject-domain `Storage` operations.

use oracle::Connection;

use crate::error::KoraError;
use crate::storage::types::HardDeleteResult;

use super::driver::{
    OraBind, append_window, b, commit_or_rollback, like_pattern, map_ora, s, scalar_bool,
    scalar_opt, to_refs,
};

pub(super) fn list_subjects(
    conn: &Connection,
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
    let (base_sql, binds): (String, Vec<OraBind>) = if let Some(pat) = like_pattern(prefix) {
        (
            format!(
                "SELECT name FROM subjects WHERE {filter} AND name LIKE :1 ESCAPE '\\' ORDER BY name"
            ),
            vec![s(&pat)],
        )
    } else {
        (
            format!("SELECT name FROM subjects WHERE {filter} ORDER BY name"),
            Vec::new(),
        )
    };
    let sql = append_window(&base_sql, offset, limit);
    let refs = to_refs(&binds);
    let mut out = Vec::new();
    for row in conn.query(&sql, &refs).map_err(map_ora)? {
        out.push(
            row.map_err(map_ora)?
                .get::<usize, String>(0)
                .map_err(map_ora)?,
        );
    }
    Ok(out)
}

pub(super) fn soft_delete_subject(conn: &Connection, name: &str) -> Result<Vec<i32>, KoraError> {
    let r = (|| -> Result<Vec<i32>, KoraError> {
        let binds = [s(name)];
        let refs = to_refs(&binds);
        let mut versions = collect_i32(
            conn,
            "SELECT sv.version FROM schema_versions sv \
             WHERE sv.subject_id = (SELECT id FROM subjects WHERE name = :1) AND sv.deleted = 0 \
             ORDER BY sv.version",
            &refs,
        )?;
        conn.execute(
            "UPDATE schema_versions SET deleted = 1 \
             WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) AND deleted = 0",
            &refs,
        )
        .map_err(map_ora)?;
        conn.execute(
            "UPDATE subjects SET deleted = 1 WHERE name = :1 AND deleted = 0",
            &refs,
        )
        .map_err(map_ora)?;
        versions.sort_unstable();
        Ok(versions)
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn hard_delete_subject(
    conn: &Connection,
    name: &str,
) -> Result<HardDeleteResult, KoraError> {
    let r = (|| -> Result<HardDeleteResult, KoraError> {
        let binds = [s(name)];
        let refs = to_refs(&binds);
        let Some(row) = conn
            .query(
                "SELECT id, deleted FROM subjects WHERE name = :1 FOR UPDATE",
                &refs,
            )
            .map_err(map_ora)?
            .next()
        else {
            return Ok(HardDeleteResult::NotFound);
        };
        let row = row.map_err(map_ora)?;
        let subject_id = row.get::<usize, i64>(0).map_err(map_ora)?;
        let deleted = row.get::<usize, i64>(1).map_err(map_ora)?;
        if deleted == 0 {
            return Ok(HardDeleteResult::NotSoftDeleted);
        }

        let mut versions = collect_i32(
            conn,
            &format!(
                "SELECT version FROM schema_versions \
                 WHERE subject_id = {subject_id} AND deleted = 1 ORDER BY version"
            ),
            &[],
        )?;

        for v in &versions {
            let referenced = scalar_bool(
                conn,
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_references sr \
                        JOIN schema_versions sv ON sr.content_id = sv.content_id \
                        WHERE sr.subject = :1 AND sr.version = {v} AND sv.deleted = 0\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &binds,
            )?;
            if referenced {
                return Ok(HardDeleteResult::ReferenceExists(format!(
                    "{name} version {v}"
                )));
            }
        }

        conn.execute(
            &format!("DELETE FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 1"),
            &[],
        )
        .map_err(map_ora)?;

        let has_active = scalar_bool(
            conn,
            &format!(
                "SELECT CASE WHEN EXISTS (\
                    SELECT 1 FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 0\
                 ) THEN 1 ELSE 0 END FROM dual"
            ),
            &[],
        )?;
        if !has_active {
            conn.execute(
                &format!("DELETE FROM subjects WHERE id = {subject_id}"),
                &[],
            )
            .map_err(map_ora)?;
        }

        versions.sort_unstable();
        Ok(HardDeleteResult::Deleted(versions))
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn find_subject_id_by_name(
    conn: &Connection,
    name: &str,
    include_deleted: bool,
) -> Result<Option<i64>, KoraError> {
    scalar_opt::<i64>(
        conn,
        "SELECT id FROM subjects WHERE name = :1 AND (deleted = 0 OR :2 = 1)",
        &[s(name), b(include_deleted)],
    )
}

pub(super) fn subject_exists(
    conn: &Connection,
    name: &str,
    include_deleted: bool,
) -> Result<bool, KoraError> {
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM subjects WHERE name = :1 AND (deleted = 0 OR :2 = 1)) \
         THEN 1 ELSE 0 END FROM dual",
        &[s(name), b(include_deleted)],
    )
}

pub(super) fn subject_is_soft_deleted(conn: &Connection, name: &str) -> Result<bool, KoraError> {
    scalar_bool(
        conn,
        "SELECT CASE WHEN EXISTS \
         (SELECT 1 FROM subjects WHERE name = :1 AND deleted = 1) THEN 1 ELSE 0 END FROM dual",
        &[s(name)],
    )
}

// -- Local helpers --

/// Collect the first (`i32`) column of an ordered query into a `Vec` (version
/// lists). Shared by the two delete paths above.
fn collect_i32(
    conn: &Connection,
    sql: &str,
    refs: &[&dyn oracle::sql_type::ToSql],
) -> Result<Vec<i32>, KoraError> {
    let mut out = Vec::new();
    for row in conn.query(sql, refs).map_err(map_ora)? {
        out.push(
            row.map_err(map_ora)?
                .get::<usize, i32>(0)
                .map_err(map_ora)?,
        );
    }
    Ok(out)
}
