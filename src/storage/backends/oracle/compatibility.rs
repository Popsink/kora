//! Oracle SQL for the compatibility-config-domain `Storage` operations.

use oracle::Connection;

use crate::error::KoraError;

use super::driver::{
    OraBind, b, commit_or_rollback, first_row, is_unique_violation, map_ora, s, scalar_bool,
    scalar_opt, scalar_opt_string, scalar_string, to_refs,
};

pub(super) fn get_subject_level(
    conn: &Connection,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    scalar_opt_string(
        conn,
        "SELECT compatibility_level FROM config \
         WHERE subject = :1 AND compatibility_level IS NOT NULL",
        &[s(subject)],
    )
}

pub(super) fn get_global_level(conn: &Connection) -> Result<String, KoraError> {
    scalar_string(
        conn,
        "SELECT COALESCE(compatibility_level, 'BACKWARD') FROM config WHERE subject IS NULL",
        &[],
    )
}

pub(super) fn set_global_level(
    conn: &Connection,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    let r = execute_one(
        conn,
        "UPDATE config SET compatibility_level = :1, normalize = :2, \
         updated_at = SYSTIMESTAMP WHERE subject IS NULL",
        &[s(level), b(normalize)],
    );
    commit_or_rollback(conn, r)?;
    Ok(level.to_owned())
}

pub(super) fn reconcile_global_level(conn: &Connection, level: &str) -> Result<String, KoraError> {
    let r = execute_one(
        conn,
        "UPDATE config SET compatibility_level = :1, updated_at = SYSTIMESTAMP \
         WHERE subject IS NULL",
        &[s(level)],
    );
    commit_or_rollback(conn, r)?;
    Ok(level.to_owned())
}

pub(super) fn set_subject_level(
    conn: &Connection,
    subject: &str,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    let r = (|| -> Result<(), KoraError> {
        let n = i64::from(normalize);
        let upd_binds = [s(level), s(subject)];
        let upd_refs = to_refs(&upd_binds);
        let upd_sql = format!(
            "UPDATE config SET compatibility_level = :1, normalize = {n}, \
             updated_at = SYSTIMESTAMP WHERE subject = :2"
        );
        let updated = conn
            .execute(&upd_sql, &upd_refs)
            .map_err(map_ora)?
            .row_count()
            .map_err(map_ora)?;
        if updated == 0 {
            // INSERT; a concurrent insert (ORA-00001) means the row now exists,
            // so re-run the UPDATE.
            let ins_binds = [s(subject), s(level)];
            let ins_refs = to_refs(&ins_binds);
            match conn.execute(
                &format!(
                    "INSERT INTO config (subject, compatibility_level, normalize) \
                     VALUES (:1, :2, {n})"
                ),
                &ins_refs,
            ) {
                Ok(_) => {}
                Err(e) if is_unique_violation(&e) => {
                    conn.execute(&upd_sql, &upd_refs).map_err(map_ora)?;
                }
                Err(e) => return Err(map_ora(e)),
            }
        }
        Ok(())
    })();
    commit_or_rollback(conn, r)?;
    Ok(level.to_owned())
}

pub(super) fn delete_subject_level(
    conn: &Connection,
    subject: &str,
) -> Result<Option<(String, bool)>, KoraError> {
    let r = (|| -> Result<Option<(String, bool)>, KoraError> {
        let binds = [s(subject)];
        let refs = to_refs(&binds);
        let result = first_level_normalize(
            conn,
            "SELECT compatibility_level, COALESCE(normalize, 0) FROM config \
             WHERE subject = :1 AND compatibility_level IS NOT NULL FOR UPDATE",
            &binds,
        )?;
        if result.is_some() {
            conn.execute(
                "UPDATE config SET compatibility_level = NULL, normalize = NULL, \
                 updated_at = SYSTIMESTAMP WHERE subject = :1",
                &refs,
            )
            .map_err(map_ora)?;
            conn.execute(
                "DELETE FROM config \
                 WHERE subject = :1 AND compatibility_level IS NULL AND registry_mode IS NULL",
                &refs,
            )
            .map_err(map_ora)?;
        }
        Ok(result)
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn get_global_normalize(conn: &Connection) -> Result<bool, KoraError> {
    scalar_bool(
        conn,
        "SELECT COALESCE(normalize, 0) FROM config WHERE subject IS NULL",
        &[],
    )
}

pub(super) fn get_subject_normalize(
    conn: &Connection,
    subject: &str,
) -> Result<Option<bool>, KoraError> {
    Ok(scalar_opt::<i64>(
        conn,
        "SELECT COALESCE(normalize, 0) FROM config \
         WHERE subject = :1 AND compatibility_level IS NOT NULL",
        &[s(subject)],
    )?
    .map(|n| n == 1))
}

pub(super) fn delete_global_level(conn: &Connection) -> Result<(String, bool), KoraError> {
    let r = (|| -> Result<(String, bool), KoraError> {
        let current = first_level_normalize(
            conn,
            "SELECT COALESCE(compatibility_level, 'BACKWARD'), COALESCE(normalize, 0) \
             FROM config WHERE subject IS NULL FOR UPDATE",
            &[],
        )?
        .unwrap_or_else(|| ("BACKWARD".to_owned(), false));
        conn.execute(
            "UPDATE config SET compatibility_level = 'BACKWARD', normalize = 0, \
             updated_at = SYSTIMESTAMP WHERE subject IS NULL",
            &[],
        )
        .map_err(map_ora)?;
        Ok(current)
    })();
    commit_or_rollback(conn, r)
}

// -- Local helpers --

/// Run a single DML statement (ignoring the affected-row count).
fn execute_one(conn: &Connection, sql: &str, binds: &[OraBind]) -> Result<(), KoraError> {
    let refs = to_refs(binds);
    conn.execute(sql, &refs).map_err(map_ora)?;
    Ok(())
}

/// Read a `(compatibility_level, normalize)` row (column 0 text, column 1 a
/// `0`/`1` flag), or `None` when there is no row.
fn first_level_normalize(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<Option<(String, bool)>, KoraError> {
    first_row(conn, sql, binds)?
        .map(|row| -> Result<(String, bool), KoraError> {
            Ok((
                row.get::<usize, String>(0).map_err(map_ora)?,
                row.get::<usize, i64>(1).map_err(map_ora)? != 0,
            ))
        })
        .transpose()
}
