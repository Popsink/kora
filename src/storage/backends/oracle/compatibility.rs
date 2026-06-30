//! Oracle SQL for the compatibility-config-domain `Storage` operations.

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::SqlExecutor;
use crate::storage::sql::helpers::{
    scalar_bool, scalar_opt_bool, scalar_opt_string, scalar_string,
};

use super::OracleStorage;
use super::driver::{is_unique_violation, s, val_i64};

pub(super) async fn get_subject_level(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<String>, KoraError> {
    scalar_opt_string(
        store,
        "SELECT compatibility_level FROM config \
         WHERE subject = :1 AND compatibility_level IS NOT NULL",
        &binds![subject],
    )
    .await
}

pub(super) async fn get_global_level(store: &OracleStorage) -> Result<String, KoraError> {
    scalar_string(
        store,
        "SELECT COALESCE(compatibility_level, 'BACKWARD') FROM config WHERE subject IS NULL",
        &[],
    )
    .await
}

pub(super) async fn set_global_level(
    store: &OracleStorage,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    store
        .execute(
            "UPDATE config SET compatibility_level = :1, normalize = :2, \
             updated_at = SYSTIMESTAMP WHERE subject IS NULL",
            &binds![level, normalize],
        )
        .await?;
    Ok(level.to_owned())
}

pub(super) async fn reconcile_global_level(
    store: &OracleStorage,
    level: &str,
) -> Result<String, KoraError> {
    store
        .execute(
            "UPDATE config SET compatibility_level = :1, updated_at = SYSTIMESTAMP \
             WHERE subject IS NULL",
            &binds![level],
        )
        .await?;
    Ok(level.to_owned())
}

pub(super) async fn set_subject_level(
    store: &OracleStorage,
    subject: &str,
    level: &str,
    normalize: bool,
) -> Result<String, KoraError> {
    let n = i64::from(normalize);
    let conn = store.conn().await?;
    let updated = conn
        .execute_dml_sql(
            &format!(
                "UPDATE config SET compatibility_level = :1, normalize = {n}, \
                 updated_at = SYSTIMESTAMP WHERE subject = :2"
            ),
            &[s(level), s(subject)],
        )
        .await?;
    if updated == 0 {
        let insert = conn
            .execute_dml_sql(
                &format!(
                    "INSERT INTO config (subject, compatibility_level, normalize) \
                     VALUES (:1, :2, {n})"
                ),
                &[s(subject), s(level)],
            )
            .await;
        match insert {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                conn.execute_dml_sql(
                    &format!(
                        "UPDATE config SET compatibility_level = :1, normalize = {n}, \
                         updated_at = SYSTIMESTAMP WHERE subject = :2"
                    ),
                    &[s(level), s(subject)],
                )
                .await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    conn.commit().await?;
    Ok(level.to_owned())
}

pub(super) async fn delete_subject_level(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<(String, bool)>, KoraError> {
    let conn = store.conn().await?;
    let current = conn
        .query(
            "SELECT compatibility_level, COALESCE(normalize, 0) FROM config \
             WHERE subject = :1 AND compatibility_level IS NOT NULL FOR UPDATE",
            &[s(subject)],
        )
        .await?;
    let result = current.first().map(|row| {
        let level = row.get_string(0).unwrap_or_default().to_owned();
        let norm = val_i64(row.get(1)).unwrap_or(0) != 0;
        (level, norm)
    });
    if result.is_some() {
        conn.execute_dml_sql(
            "UPDATE config SET compatibility_level = NULL, normalize = NULL, \
             updated_at = SYSTIMESTAMP WHERE subject = :1",
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
    Ok(result)
}

pub(super) async fn get_global_normalize(store: &OracleStorage) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT COALESCE(normalize, 0) FROM config WHERE subject IS NULL",
        &[],
    )
    .await
}

pub(super) async fn get_subject_normalize(
    store: &OracleStorage,
    subject: &str,
) -> Result<Option<bool>, KoraError> {
    scalar_opt_bool(
        store,
        "SELECT COALESCE(normalize, 0) FROM config \
         WHERE subject = :1 AND compatibility_level IS NOT NULL",
        &binds![subject],
    )
    .await
}

pub(super) async fn delete_global_level(
    store: &OracleStorage,
) -> Result<(String, bool), KoraError> {
    let conn = store.conn().await?;
    let current = conn
        .query(
            "SELECT COALESCE(compatibility_level, 'BACKWARD'), COALESCE(normalize, 0) \
             FROM config WHERE subject IS NULL FOR UPDATE",
            &[],
        )
        .await?;
    let (level, normalize) = current.first().map_or_else(
        || ("BACKWARD".to_owned(), false),
        |row| {
            (
                row.get_string(0).unwrap_or("BACKWARD").to_owned(),
                val_i64(row.get(1)).unwrap_or(0) != 0,
            )
        },
    );
    conn.execute_dml_sql(
        "UPDATE config SET compatibility_level = 'BACKWARD', normalize = 0, \
         updated_at = SYSTIMESTAMP WHERE subject IS NULL",
        &[],
    )
    .await?;
    conn.commit().await?;
    Ok((level, normalize))
}
