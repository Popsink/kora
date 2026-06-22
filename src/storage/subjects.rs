//! Subject storage operations.

use sqlx::{AssertSqlSafe, PgPool};

// -- Queries --

/// List subject names, sorted alphabetically, with pagination.
///
/// - `include_deleted`: when true, include soft-deleted subjects in results.
/// - `deleted_only`: when true, return ONLY soft-deleted subjects (takes precedence).
/// - `prefix`: when `Some`, filter to subjects whose name starts with this prefix.
/// - `offset` defaults to 0, `limit` of -1 means unlimited.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn list_subjects(
    pool: &PgPool,
    include_deleted: bool,
    deleted_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let like_pattern = prefix.filter(|p| !p.is_empty()).map(|p| {
        let escaped = p
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("{escaped}%")
    });

    // Build query with literal WHERE clause so PG can use partial indexes.
    // Using bind parameters for the deleted filter prevents index usage.
    let filter = if deleted_only {
        "deleted = true"
    } else if include_deleted {
        "true"
    } else {
        "deleted = false"
    };

    let (sql, has_like) = match (&like_pattern, limit >= 0) {
        (Some(_), true) => (
            format!(
                "SELECT name FROM subjects WHERE {filter} AND name LIKE $1 ESCAPE '\\' ORDER BY name OFFSET $2 LIMIT $3"
            ),
            true,
        ),
        (Some(_), false) => (
            format!(
                "SELECT name FROM subjects WHERE {filter} AND name LIKE $1 ESCAPE '\\' ORDER BY name OFFSET $2"
            ),
            true,
        ),
        (None, true) => (
            format!("SELECT name FROM subjects WHERE {filter} ORDER BY name OFFSET $1 LIMIT $2"),
            false,
        ),
        (None, false) => (
            format!("SELECT name FROM subjects WHERE {filter} ORDER BY name OFFSET $1"),
            false,
        ),
    };

    // SAFETY (sqlx 0.9 `SqlSafeStr`): the only value interpolated into `sql` is
    // `filter`, which is one of three hardcoded literals above. Everything
    // derived from input — the LIKE pattern, offset, limit — is passed as a bind
    // parameter, never spliced into the string. The query text contains no
    // untrusted data, so wrapping it with `AssertSqlSafe` is sound.
    if has_like {
        let pat = like_pattern.as_deref().unwrap_or("%");
        if limit >= 0 {
            sqlx::query_scalar(AssertSqlSafe(sql))
                .bind(pat)
                .bind(offset)
                .bind(limit)
                .fetch_all(pool)
                .await
        } else {
            sqlx::query_scalar(AssertSqlSafe(sql))
                .bind(pat)
                .bind(offset)
                .fetch_all(pool)
                .await
        }
    } else if limit >= 0 {
        sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(offset)
            .fetch_all(pool)
            .await
    }
}

/// Soft-delete a subject and all its schema versions. Returns the deleted version
/// numbers sorted ascending. Runs in a transaction for consistency.
///
/// # Errors
///
/// Returns a database error on connection or transaction failure.
pub async fn soft_delete_subject(pool: &PgPool, name: &str) -> Result<Vec<i32>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let mut versions = sqlx::query_scalar::<_, i32>(
        r"UPDATE schema_versions SET deleted = true
           WHERE subject_id = (SELECT id FROM subjects WHERE name = $1) AND deleted = false
           RETURNING version",
    )
    .bind(name)
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query("UPDATE subjects SET deleted = true WHERE name = $1 AND deleted = false")
        .bind(name)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    versions.sort_unstable();
    Ok(versions)
}

/// Hard-delete result with enough context for the handler to return the right error.
pub enum HardDeleteResult {
    /// Subject deleted, returns sorted version numbers.
    Deleted(Vec<i32>),
    /// Subject exists but is NOT soft-deleted (active).
    NotSoftDeleted,
    /// Subject does not exist.
    NotFound,
    /// A referenced version blocks deletion.
    ReferenceExists(String),
}

/// Hard-delete a subject atomically: lock the row, verify preconditions
/// (must be soft-deleted, no referenced versions), then delete.
///
/// All checks run inside the transaction to eliminate TOCTOU races
/// with concurrent writers that could re-activate the subject.
///
/// # Errors
///
/// Returns a database error on connection or transaction failure.
pub async fn hard_delete_subject(
    pool: &PgPool,
    name: &str,
) -> Result<HardDeleteResult, sqlx::Error> {
    let mut tx = pool.begin().await?;

    // Lock the subject row to prevent concurrent modifications.
    let Some((subject_id, deleted)) = sqlx::query_as::<_, (i64, bool)>(
        "SELECT id, deleted FROM subjects WHERE name = $1 FOR UPDATE",
    )
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(HardDeleteResult::NotFound);
    };

    if !deleted {
        return Ok(HardDeleteResult::NotSoftDeleted);
    }

    // Check references inside the transaction (no TOCTOU).
    let versions: Vec<i32> = sqlx::query_scalar(
        "SELECT version FROM schema_versions WHERE subject_id = $1 AND deleted = true",
    )
    .bind(subject_id)
    .fetch_all(&mut *tx)
    .await?;

    for v in &versions {
        let is_referenced: bool = sqlx::query_scalar(
            r"SELECT EXISTS(
                SELECT 1 FROM schema_references sr
                JOIN schema_versions sv ON sr.content_id = sv.content_id
                WHERE sr.subject = $1 AND sr.version = $2 AND sv.deleted = false
            )",
        )
        .bind(name)
        .bind(v)
        .fetch_one(&mut *tx)
        .await?;

        if is_referenced {
            return Ok(HardDeleteResult::ReferenceExists(format!(
                "{name} version {v}"
            )));
        }
    }

    // Delete soft-deleted versions.
    sqlx::query("DELETE FROM schema_versions WHERE subject_id = $1 AND deleted = true")
        .bind(subject_id)
        .execute(&mut *tx)
        .await?;

    // Only delete the subject if no active versions remain (a concurrent writer
    // may have inserted a new version between our lock and this point via the
    // UPSERT which re-activates the subject).
    let has_active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM schema_versions WHERE subject_id = $1 AND deleted = false)",
    )
    .bind(subject_id)
    .fetch_one(&mut *tx)
    .await?;

    if !has_active {
        sqlx::query("DELETE FROM subjects WHERE id = $1")
            .bind(subject_id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;

    let mut sorted = versions;
    sorted.sort_unstable();
    Ok(HardDeleteResult::Deleted(sorted))
}

/// Find a subject's ID by name, optionally including soft-deleted subjects.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_subject_id_by_name(
    pool: &PgPool,
    name: &str,
    include_deleted: bool,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM subjects WHERE name = $1 AND (deleted = false OR $2)",
    )
    .bind(name)
    .bind(include_deleted)
    .fetch_optional(pool)
    .await
}

/// Check if a subject exists by name, optionally including soft-deleted subjects.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn subject_exists(
    pool: &PgPool,
    name: &str,
    include_deleted: bool,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE name = $1 AND (deleted = false OR $2))",
    )
    .bind(name)
    .bind(include_deleted)
    .fetch_one(pool)
    .await
}

/// Check if a subject is soft-deleted.
///
/// Returns true if subject exists AND is soft-deleted.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn subject_is_soft_deleted(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM subjects WHERE name = $1 AND deleted = true)",
    )
    .bind(name)
    .fetch_one(pool)
    .await
}
