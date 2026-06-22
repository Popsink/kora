//! Compatibility configuration storage operations.

use sqlx::PgPool;

// -- Queries --

/// Get the per-subject compatibility level only (no fallback).
///
/// Returns `None` if no per-subject config exists or compatibility is not configured.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_subject_level(
    pool: &PgPool,
    subject: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT compatibility_level FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL",
    )
    .bind(subject)
    .fetch_optional(pool)
    .await
}

/// Get the global compatibility level.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_global_level(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(compatibility_level, 'BACKWARD') FROM config WHERE subject IS NULL",
    )
    .fetch_one(pool)
    .await
}

/// Update the global compatibility level and return the new value.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn set_global_level(
    pool: &PgPool,
    level: &str,
    normalize: bool,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r"UPDATE config SET compatibility_level = $1, normalize = $2, updated_at = now()
          WHERE subject IS NULL
          RETURNING compatibility_level",
    )
    .bind(level)
    .bind(normalize)
    .fetch_one(pool)
    .await
}

/// Reconcile the global compatibility level to `level`, returning the new value.
///
/// Unconditionally overwrites the global row (`subject IS NULL`); used at startup
/// to make a declaratively-configured default (`DEFAULT_COMPATIBILITY`) the source
/// of truth on every boot. Leaves `normalize` and all per-subject overrides
/// untouched.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn reconcile_global_level(pool: &PgPool, level: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r"UPDATE config SET compatibility_level = $1, updated_at = now()
          WHERE subject IS NULL
          RETURNING compatibility_level",
    )
    .bind(level)
    .fetch_one(pool)
    .await
}

/// Set the per-subject compatibility level (upsert). Returns the new value.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn set_subject_level(
    pool: &PgPool,
    subject: &str,
    level: &str,
    normalize: bool,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r"INSERT INTO config (subject, compatibility_level, normalize)
          VALUES ($1, $2, $3)
          ON CONFLICT (subject) DO UPDATE SET compatibility_level = $2, normalize = $3, updated_at = now()
          RETURNING compatibility_level",
    )
    .bind(subject)
    .bind(level)
    .bind(normalize)
    .fetch_one(pool)
    .await
}

/// Delete per-subject compatibility config by setting it to NULL.
///
/// Returns the **previous** `(level, normalize)`, or `None` if not configured.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn delete_subject_level(
    pool: &PgPool,
    subject: &str,
) -> Result<Option<(String, bool)>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let row = sqlx::query(
        "SELECT compatibility_level, COALESCE(normalize, false) AS normalize FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL FOR UPDATE",
    )
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;

    let result = row.map(|r| {
        let level: String = sqlx::Row::get(&r, "compatibility_level");
        let normalize: bool = sqlx::Row::get(&r, "normalize");
        (level, normalize)
    });

    if result.is_some() {
        // Reset compat fields to NULL; delete row if mode is also NULL.
        sqlx::query(
            "UPDATE config SET compatibility_level = NULL, normalize = NULL, updated_at = now() WHERE subject = $1",
        )
        .bind(subject)
        .execute(&mut *tx)
        .await?;

        // Clean up orphan row (all nullable fields are NULL).
        sqlx::query(
            "DELETE FROM config WHERE subject = $1 AND compatibility_level IS NULL AND mode IS NULL",
        )
        .bind(subject)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(result)
}

/// Get the global normalize setting.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_global_normalize(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(normalize, false) FROM config WHERE subject IS NULL",
    )
    .fetch_one(pool)
    .await
}

/// Get the subject-level normalize setting (no fallback).
///
/// Returns `None` if no per-subject compatibility config exists.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_subject_normalize(
    pool: &PgPool,
    subject: &str,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(normalize, false) FROM config WHERE subject = $1 AND compatibility_level IS NOT NULL",
    )
    .bind(subject)
    .fetch_optional(pool)
    .await
}

/// Get the effective normalize setting for a subject (subject-level, then global fallback).
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_effective_normalize(pool: &PgPool, subject: &str) -> Result<bool, sqlx::Error> {
    if let Some(n) = get_subject_normalize(pool, subject).await? {
        return Ok(n);
    }
    get_global_normalize(pool).await
}

/// Get the effective compatibility level for a subject (subject-level, then global fallback).
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn get_effective_compatibility(
    pool: &PgPool,
    subject: &str,
) -> Result<String, sqlx::Error> {
    if let Some(level) = get_subject_level(pool, subject).await? {
        return Ok(level);
    }
    get_global_level(pool).await
}

/// Delete (reset) the global compatibility level to BACKWARD (default).
///
/// Returns the **previous** `(compatibility_level, normalize)` before the reset.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn delete_global_level(pool: &PgPool) -> Result<(String, bool), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let row = sqlx::query(
        "SELECT COALESCE(compatibility_level, 'BACKWARD') AS compatibility_level, COALESCE(normalize, false) AS normalize FROM config WHERE subject IS NULL FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;

    let prev_level: String = sqlx::Row::get(&row, "compatibility_level");
    let prev_normalize: bool = sqlx::Row::get(&row, "normalize");

    sqlx::query(
        "UPDATE config SET compatibility_level = 'BACKWARD', normalize = false, updated_at = now() WHERE subject IS NULL",
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((prev_level, prev_normalize))
}
