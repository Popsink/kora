//! Oracle SQL for the registry-mode-domain `Storage` operations.
//!
//! The mode column is named `registry_mode` here because `MODE` is an Oracle
//! reserved word (see the module-level docs on dialect translation).

use oracle::Connection;

use crate::error::KoraError;

use super::driver::{
    OraBind, commit_or_rollback, is_unique_violation, map_ora, s, scalar_opt_string, scalar_string,
    to_refs,
};

pub(super) fn get_global_mode(conn: &Connection) -> Result<String, KoraError> {
    scalar_string(
        conn,
        "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL",
        &[],
    )
}

pub(super) fn set_global_mode(conn: &Connection, mode: &str) -> Result<String, KoraError> {
    let r = execute_one(
        conn,
        "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject IS NULL",
        &[s(mode)],
    );
    commit_or_rollback(conn, r)?;
    Ok(mode.to_owned())
}

pub(super) fn delete_global_mode(conn: &Connection) -> Result<String, KoraError> {
    let r = (|| -> Result<String, KoraError> {
        let prev = scalar_string(
            conn,
            "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL FOR UPDATE",
            &[],
        )?;
        conn.execute(
            "UPDATE config SET registry_mode = 'READWRITE', updated_at = SYSTIMESTAMP \
             WHERE subject IS NULL",
            &[],
        )
        .map_err(map_ora)?;
        Ok(prev)
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn get_subject_mode(
    conn: &Connection,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    scalar_opt_string(
        conn,
        "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL",
        &[s(subject)],
    )
}

pub(super) fn set_subject_mode(
    conn: &Connection,
    subject: &str,
    mode: &str,
) -> Result<String, KoraError> {
    let r = (|| -> Result<(), KoraError> {
        let upd_binds = [s(mode), s(subject)];
        let upd_refs = to_refs(&upd_binds);
        let upd_sql =
            "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject = :2";
        let updated = conn
            .execute(upd_sql, &upd_refs)
            .map_err(map_ora)?
            .row_count()
            .map_err(map_ora)?;
        if updated == 0 {
            // INSERT; a concurrent insert (ORA-00001) means the row now exists,
            // so re-run the UPDATE.
            let ins_binds = [s(subject), s(mode)];
            let ins_refs = to_refs(&ins_binds);
            match conn.execute(
                "INSERT INTO config (subject, registry_mode) VALUES (:1, :2)",
                &ins_refs,
            ) {
                Ok(_) => {}
                Err(e) if is_unique_violation(&e) => {
                    conn.execute(upd_sql, &upd_refs).map_err(map_ora)?;
                }
                Err(e) => return Err(map_ora(e)),
            }
        }
        Ok(())
    })();
    commit_or_rollback(conn, r)?;
    Ok(mode.to_owned())
}

pub(super) fn delete_subject_mode(
    conn: &Connection,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let r = (|| -> Result<Option<String>, KoraError> {
        let binds = [s(subject)];
        let refs = to_refs(&binds);
        let prev = scalar_opt_string(
            conn,
            "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
            &binds,
        )?;
        if prev.is_some() {
            conn.execute(
                "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
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
        Ok(prev)
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn delete_subject_mode_recursive(
    conn: &Connection,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let r = (|| -> Result<Option<String>, KoraError> {
        let one = [s(subject)];
        let one_refs = to_refs(&one);
        let prev = scalar_opt_string(
            conn,
            "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
            &one,
        )?;
        if prev.is_some() {
            conn.execute(
                "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
                &one_refs,
            )
            .map_err(map_ora)?;
        }
        // Children: starts-with via INSTR (no LIKE-wildcard injection).
        let two = [s(subject), s(subject)];
        let two_refs = to_refs(&two);
        conn.execute(
            "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP \
             WHERE INSTR(subject, :1) = 1 AND subject != :2 AND registry_mode IS NOT NULL",
            &two_refs,
        )
        .map_err(map_ora)?;
        let three = [s(subject), s(subject), s(subject)];
        let three_refs = to_refs(&three);
        conn.execute(
            "DELETE FROM config \
             WHERE (subject = :1 OR (INSTR(subject, :2) = 1 AND subject != :3)) \
               AND compatibility_level IS NULL AND registry_mode IS NULL",
            &three_refs,
        )
        .map_err(map_ora)?;
        Ok(prev)
    })();
    commit_or_rollback(conn, r)
}

pub(super) fn get_effective_mode(conn: &Connection, subject: &str) -> Result<String, KoraError> {
    scalar_string(
        conn,
        "SELECT COALESCE(\
            (SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL), \
            (SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL)\
         ) FROM dual",
        &[s(subject)],
    )
}

// -- Local helpers --

/// Run a single DML statement (ignoring the affected-row count).
fn execute_one(conn: &Connection, sql: &str, binds: &[OraBind]) -> Result<(), KoraError> {
    let refs = to_refs(binds);
    conn.execute(sql, &refs).map_err(map_ora)?;
    Ok(())
}
