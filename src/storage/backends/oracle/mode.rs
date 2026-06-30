//! Oracle SQL for the registry-mode-domain `Storage` operations.
//!
//! The mode column is named `registry_mode` here because `MODE` is an Oracle
//! reserved word (see the module-level docs on dialect translation).

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::SqlExecutor;
use crate::storage::sql::helpers::{scalar_opt_string, scalar_string};

use super::OracleStorage;
use super::driver::{is_unique_violation, s};

pub(super) async fn get_global_mode(store: &OracleStorage) -> Result<String, KoraError> {
    scalar_string(
        store,
        "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL",
        &[],
    )
    .await
}

pub(super) async fn set_global_mode(
    store: &OracleStorage,
    mode: &str,
) -> Result<String, KoraError> {
    store
        .execute(
            "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject IS NULL",
            &binds![mode],
        )
        .await?;
    Ok(mode.to_owned())
}

pub(super) async fn delete_global_mode(store: &OracleStorage) -> Result<String, KoraError> {
    let conn = store.conn().await?;
    let current = conn
        .query(
            "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL FOR UPDATE",
            &[],
        )
        .await?;
    let prev = current
        .first()
        .and_then(|r| r.get_string(0))
        .unwrap_or("READWRITE")
        .to_owned();
    conn.execute_dml_sql(
        "UPDATE config SET registry_mode = 'READWRITE', updated_at = SYSTIMESTAMP \
         WHERE subject IS NULL",
        &[],
    )
    .await?;
    conn.commit().await?;
    Ok(prev)
}

pub(super) async fn get_subject_mode(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    scalar_opt_string(
        store,
        "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL",
        &binds![subject],
    )
    .await
}

pub(super) async fn set_subject_mode(
    store: &OracleStorage,
    subject: &str,
    mode: &str,
) -> Result<String, KoraError> {
    let conn = store.conn().await?;
    let updated = conn
        .execute_dml_sql(
            "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject = :2",
            &[s(mode), s(subject)],
        )
        .await?;
    if updated == 0 {
        let insert = conn
            .execute_dml_sql(
                "INSERT INTO config (subject, registry_mode) VALUES (:1, :2)",
                &[s(subject), s(mode)],
            )
            .await;
        match insert {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                conn.execute_dml_sql(
                    "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP \
                     WHERE subject = :2",
                    &[s(mode), s(subject)],
                )
                .await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    conn.commit().await?;
    Ok(mode.to_owned())
}

pub(super) async fn delete_subject_mode(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let conn = store.conn().await?;
    let current = conn
        .query(
            "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
            &[s(subject)],
        )
        .await?;
    let prev = current
        .first()
        .and_then(|r| r.get_string(0))
        .map(str::to_owned);
    if prev.is_some() {
        conn.execute_dml_sql(
            "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
            &[s(subject)],
        )
        .await?;
        conn.execute_dml_sql(
            "DELETE FROM config \
             WHERE subject = :1 AND compatibility_level IS NULL AND registry_mode IS NULL",
            &[s(subject)],
        )
        .await?;
    }
    conn.commit().await?;
    Ok(prev)
}

pub(super) async fn delete_subject_mode_recursive(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    let conn = store.conn().await?;
    let current = conn
        .query(
            "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
            &[s(subject)],
        )
        .await?;
    let prev = current
        .first()
        .and_then(|r| r.get_string(0))
        .map(str::to_owned);
    if prev.is_some() {
        conn.execute_dml_sql(
            "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
            &[s(subject)],
        )
        .await?;
    }
    // Children: starts-with via INSTR (no LIKE-wildcard injection).
    conn.execute_dml_sql(
        "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP \
         WHERE INSTR(subject, :1) = 1 AND subject != :2 AND registry_mode IS NOT NULL",
        &[s(subject), s(subject)],
    )
    .await?;
    conn.execute_dml_sql(
        "DELETE FROM config \
         WHERE (subject = :1 OR (INSTR(subject, :2) = 1 AND subject != :3)) \
           AND compatibility_level IS NULL AND registry_mode IS NULL",
        &[s(subject), s(subject), s(subject)],
    )
    .await?;
    conn.commit().await?;
    Ok(prev)
}

pub(super) async fn get_effective_mode(
    store: &OracleStorage,
    subject: &str,
) -> Result<String, KoraError> {
    scalar_string(
        store,
        "SELECT COALESCE(\
            (SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL), \
            (SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL)\
         ) FROM dual",
        &binds![subject],
    )
    .await
}
