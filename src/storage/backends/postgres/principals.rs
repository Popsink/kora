//! Principal (authentication) operations for the `PostgreSQL` backend.

use sqlx::types::Json;

use crate::auth::{Role, Rule};
use crate::error::KoraError;
use crate::storage::types::{NewPrincipal, PrincipalRecord, PrincipalUpdate};

use super::PgStorage;

/// Columns selected for a [`PrincipalRecord`], timestamps rendered as RFC 3339 UTC.
const COLUMNS: &str = r#"id, name, password_hash, role, rules,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')"#;

type PrincipalRow = (i64, String, String, String, Json<Vec<Rule>>, String, String);

fn record_from_row(
    (id, name, password_hash, role, rules, created_at, updated_at): PrincipalRow,
) -> Result<PrincipalRecord, KoraError> {
    Ok(PrincipalRecord {
        id,
        name,
        password_hash,
        role: Role::parse(&role)?,
        rules: rules.0,
        created_at,
        updated_at,
    })
}

pub(super) async fn list_principals(store: &PgStorage) -> Result<Vec<PrincipalRecord>, KoraError> {
    // SAFETY (sqlx 0.9 `SqlSafeStr`): `COLUMNS` is a hardcoded literal.
    let rows: Vec<PrincipalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM principals ORDER BY name"
    )))
    .fetch_all(store.pool())
    .await?;
    rows.into_iter().map(record_from_row).collect()
}

pub(super) async fn find_principal(
    store: &PgStorage,
    name: &str,
) -> Result<Option<PrincipalRecord>, KoraError> {
    let row: Option<PrincipalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM principals WHERE name = $1"
    )))
    .bind(name)
    .fetch_optional(store.pool())
    .await?;
    row.map(record_from_row).transpose()
}

pub(super) async fn create_principal(
    store: &PgStorage,
    new: &NewPrincipal<'_>,
) -> Result<Option<PrincipalRecord>, KoraError> {
    let row: Option<PrincipalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO principals (name, password_hash, role, rules) VALUES ($1, $2, $3, $4)
         ON CONFLICT (name) DO NOTHING
         RETURNING {COLUMNS}"
    )))
    .bind(new.name)
    .bind(new.password_hash)
    .bind(new.role.as_str())
    .bind(Json(new.rules))
    .fetch_optional(store.pool())
    .await?;
    row.map(record_from_row).transpose()
}

pub(super) async fn update_principal(
    store: &PgStorage,
    name: &str,
    update: &PrincipalUpdate<'_>,
) -> Result<Option<PrincipalRecord>, KoraError> {
    let row: Option<PrincipalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE principals SET
             password_hash = COALESCE($2, password_hash),
             role = COALESCE($3, role),
             rules = COALESCE($4, rules),
             updated_at = now()
         WHERE name = $1
         RETURNING {COLUMNS}"
    )))
    .bind(name)
    .bind(update.password_hash)
    .bind(update.role.map(Role::as_str))
    .bind(update.rules.map(Json))
    .fetch_optional(store.pool())
    .await?;
    row.map(record_from_row).transpose()
}

pub(super) async fn delete_principal(
    store: &PgStorage,
    name: &str,
) -> Result<Option<i64>, KoraError> {
    Ok(
        sqlx::query_scalar("DELETE FROM principals WHERE name = $1 RETURNING id")
            .bind(name)
            .fetch_optional(store.pool())
            .await?,
    )
}
