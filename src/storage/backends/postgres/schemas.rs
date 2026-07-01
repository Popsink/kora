//! Schema and version operations for the `PostgreSQL` backend.
//!
//! Simple reads run through the SQL toolkit and the shared [`row_to_sv`] mapper;
//! the atomic registration path runs in a transaction over the raw
//! [`PgPool`](sqlx::PgPool) and owns its dialect SQL (plus its private helpers)
//! verbatim.
//!
//! [`row_to_sv`]: super::row_to_sv

use crate::binds;
use crate::error::KoraError;
use crate::storage::sql::helpers::{fetch_strings, scalar_bool, scalar_opt_i64};
use crate::storage::sql::{Row, SqlExecutor};
use crate::storage::types::{CompatCheck, NewSchema, SchemaVersion, SubjectVersion};
use crate::types::SchemaReference;

use super::{PgStorage, like_pattern, row_to_sv};

pub(super) async fn find_all_active_versions(
    store: &PgStorage,
    subject: &str,
) -> Result<Vec<SchemaVersion>, KoraError> {
    store
        .fetch_all(
            r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sub.name = $1 AND sv.deleted = false
               ORDER BY sv.version",
            &binds![subject],
        )
        .await?
        .iter()
        .map(row_to_sv)
        .collect()
}

pub(super) async fn find_schema_by_subject_version(
    store: &PgStorage,
    subject: &str,
    version: i32,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let sql = if include_deleted {
        r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.version = $2"
    } else {
        r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = false"
    };
    match store.fetch_optional(sql, &binds![subject, version]).await? {
        Some(r) => Ok(Some(row_to_sv(&r)?)),
        None => Ok(None),
    }
}

pub(super) async fn find_latest_schema_by_subject(
    store: &PgStorage,
    subject: &str,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let sql = if include_deleted {
        r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1
           ORDER BY sv.version DESC LIMIT 1"
    } else {
        r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.deleted = false
           ORDER BY sv.version DESC LIMIT 1"
    };
    match store.fetch_optional(sql, &binds![subject]).await? {
        Some(r) => Ok(Some(row_to_sv(&r)?)),
        None => Ok(None),
    }
}

pub(super) async fn find_schema_by_subject_id_and_fingerprint(
    store: &PgStorage,
    subject_id: i64,
    fingerprint: &str,
    normalize: bool,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, KoraError> {
    let sql = match (normalize, include_deleted) {
        (true, true) => {
            r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.fingerprint = $2"
        }
        (true, false) => {
            r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.fingerprint = $2 AND sv.deleted = false"
        }
        (false, true) => {
            r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.raw_fingerprint = $2"
        }
        (false, false) => {
            r"SELECT sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.raw_fingerprint = $2 AND sv.deleted = false"
        }
    };
    match store
        .fetch_optional(sql, &binds![subject_id, fingerprint])
        .await?
    {
        Some(r) => Ok(Some(row_to_sv(&r)?)),
        None => Ok(None),
    }
}

pub(super) async fn find_schema_by_id(
    store: &PgStorage,
    id: i64,
) -> Result<Option<(String, String)>, KoraError> {
    match store
        .fetch_optional(
            "SELECT schema_text, schema_type FROM schema_contents WHERE id = $1",
            &binds![id],
        )
        .await?
    {
        Some(r) => Ok(Some((r.get_str(0)?, r.get_str(1)?))),
        None => Ok(None),
    }
}

pub(super) async fn find_max_schema_id(store: &PgStorage) -> Result<i64, KoraError> {
    Ok(
        scalar_opt_i64(store, "SELECT MAX(id) FROM schema_contents", &[])
            .await?
            .unwrap_or(0),
    )
}

pub(super) async fn schema_exists(store: &PgStorage, id: i64) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        "SELECT EXISTS(SELECT 1 FROM schema_contents WHERE id = $1)",
        &binds![id],
    )
    .await
}

pub(super) async fn find_subjects_by_schema_id(
    store: &PgStorage,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, KoraError> {
    if let Some(filter) = subject_filter {
        let sql = r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name";
        fetch_strings(
            store,
            sql,
            &binds![id, include_deleted, filter],
            offset,
            limit,
        )
        .await
    } else {
        let sql = r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name";
        fetch_strings(store, sql, &binds![id, include_deleted], offset, limit).await
    }
}

pub(super) async fn find_versions_by_schema_id(
    store: &PgStorage,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SubjectVersion>, KoraError> {
    let rows = if let Some(filter) = subject_filter {
        let sql = r"SELECT sub.name, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name, sv.version";
        store
            .fetch_all_paged(sql, &binds![id, include_deleted, filter], offset, limit)
            .await?
    } else {
        let sql = r"SELECT sub.name, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name, sv.version";
        store
            .fetch_all_paged(sql, &binds![id, include_deleted], offset, limit)
            .await?
    };
    rows.iter()
        .map(|r| {
            Ok(SubjectVersion {
                subject: r.get_str(0)?,
                version: r.get_i32(1)?,
            })
        })
        .collect()
}

pub(super) async fn list_schema_versions(
    store: &PgStorage,
    subject: &str,
    include_deleted: bool,
    deleted_only: bool,
    deleted_as_negative: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i32>, KoraError> {
    let rows = if deleted_only && deleted_as_negative {
        let sql = r"SELECT -sv.version AS version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1 AND sv.deleted = true
               ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else if deleted_only {
        let sql = r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1 AND sv.deleted = true
               ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else if deleted_as_negative && include_deleted {
        let sql = r"SELECT CASE WHEN sv.deleted THEN -sv.version ELSE sv.version END AS version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1
               ORDER BY abs(sv.version)";
        store
            .fetch_all_paged(sql, &binds![subject], offset, limit)
            .await?
    } else {
        let sql = r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1 AND (sv.deleted = false OR $2)
               ORDER BY sv.version";
        store
            .fetch_all_paged(sql, &binds![subject, include_deleted], offset, limit)
            .await?
    };
    rows.iter().map(|r| r.get_i32(0)).collect()
}

pub(super) async fn list_schemas(
    store: &PgStorage,
    include_deleted: bool,
    latest_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let like = like_pattern(prefix);
    let rows = match (latest_only, &like) {
        (true, Some(pat)) => {
            let sql = r"SELECT DISTINCT ON (sub.name) sc.id, sub.name, sv.version,
                     sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version DESC";
            store
                .fetch_all_paged(sql, &binds![include_deleted, pat], offset, limit)
                .await?
        }
        (true, None) => {
            let sql = r"SELECT DISTINCT ON (sub.name) sc.id, sub.name, sv.version,
                     sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version DESC";
            store
                .fetch_all_paged(sql, &binds![include_deleted], offset, limit)
                .await?
        }
        (false, Some(pat)) => {
            let sql = r"SELECT sc.id, sub.name, sv.version,
                     sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version";
            store
                .fetch_all_paged(sql, &binds![include_deleted, pat], offset, limit)
                .await?
        }
        (false, None) => {
            let sql = r"SELECT sc.id, sub.name, sv.version,
                     sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version";
            store
                .fetch_all_paged(sql, &binds![include_deleted], offset, limit)
                .await?
        }
    };
    rows.iter().map(row_to_sv).collect()
}

pub(super) async fn soft_delete_latest_schema(
    store: &PgStorage,
    subject: &str,
) -> Result<Option<i32>, KoraError> {
    match store
        .fetch_optional(
            r"UPDATE schema_versions SET deleted = true
               WHERE id = (
                 SELECT sv.id FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                 WHERE sub.name = $1 AND sv.deleted = false
                 ORDER BY sv.version DESC LIMIT 1
               )
               RETURNING version",
            &binds![subject],
        )
        .await?
    {
        Some(r) => Ok(Some(r.get_i32(0)?)),
        None => Ok(None),
    }
}

pub(super) async fn soft_delete_schema_version(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    match store
        .fetch_optional(
            r"UPDATE schema_versions SET deleted = true
               WHERE subject_id = (SELECT id FROM subjects WHERE name = $1)
                 AND version = $2 AND deleted = false
               RETURNING version",
            &binds![subject, version],
        )
        .await?
    {
        Some(r) => Ok(Some(r.get_i32(0)?)),
        None => Ok(None),
    }
}

pub(super) async fn hard_delete_schema_version(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, KoraError> {
    match store
        .fetch_optional(
            r"DELETE FROM schema_versions
               WHERE subject_id = (SELECT id FROM subjects WHERE name = $1)
                 AND version = $2 AND deleted = true
               RETURNING version",
            &binds![subject, version],
        )
        .await?
    {
        Some(r) => Ok(Some(r.get_i32(0)?)),
        None => Ok(None),
    }
}

pub(super) async fn version_is_soft_deleted(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        r"SELECT EXISTS(
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
            WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = true
        )",
        &binds![subject, version],
    )
    .await
}

pub(super) async fn version_is_active(
    store: &PgStorage,
    subject: &str,
    version: i32,
) -> Result<bool, KoraError> {
    scalar_bool(
        store,
        r"SELECT EXISTS(
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
            WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = false
        )",
        &binds![subject, version],
    )
    .await
}

// -- Transactional operations --
//
// The multi-statement registration path must run in a single transaction, which
// the toolkit's single-statement `SqlExecutor` cannot express; it runs over the
// raw `PgPool` and owns its dialect SQL verbatim, alongside its private helpers.

/// Register a schema atomically: upsert subject, deduplicate content globally,
/// create version, and store references — all in a single transaction.
///
/// The compatibility check runs **inside** the transaction, after acquiring the
/// subject row lock. This eliminates the TOCTOU race where two concurrent
/// registrations could both pass the check against stale data.
///
/// Content dedup is global: identical schema text shares one `schema_contents` row
/// and one global ID across all subjects (Confluent behavior). The UNIQUE constraint
/// on `raw_fingerprint` prevents duplicate rows under concurrent inserts.
///
/// Returns `(content_id, version, is_new)` — if `is_new` is false, the schema was
/// already registered under this subject (idempotent).
pub(super) async fn register_schema_atomically(
    store: &PgStorage,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[SchemaReference],
    normalize: bool,
    compat: Option<CompatCheck>,
) -> Result<(i64, i32, bool), KoraError> {
    let mut tx = store.pool().begin().await?;

    // Upsert subject and lock the row — re-activates soft-deleted subjects.
    // ON CONFLICT DO UPDATE implicitly acquires a row-level lock on the
    // conflicting row, so a separate SELECT ... FOR UPDATE is unnecessary.
    let subject_id = sqlx::query_scalar::<_, i64>(
        r"INSERT INTO subjects (name) VALUES ($1)
          ON CONFLICT (name) DO UPDATE SET deleted = false, updated_at = now()
          RETURNING id",
    )
    .bind(subject_name)
    .fetch_one(&mut *tx)
    .await?;

    // Per-subject idempotency: does this subject already have an active version
    // pointing to content with this fingerprint?
    let fp = if normalize {
        schema.fingerprint
    } else {
        schema.raw_fingerprint
    };
    let fp_col = if normalize {
        "fingerprint"
    } else {
        "raw_fingerprint"
    };

    if let Some((content_id, version_num)) =
        find_existing_version(&mut tx, subject_id, fp, fp_col).await?
    {
        tx.commit().await?;
        return Ok((content_id, version_num, false));
    }

    // Run compatibility check inside the transaction, after locking the subject.
    // This guarantees no other registration can insert between check and insert.
    if let Some(compat) = compat {
        run_compat_check(&mut tx, subject_id, compat).await?;
    }

    // Global content dedup: INSERT with ON CONFLICT on the UNIQUE raw_fingerprint.
    // This safely handles concurrent inserts of identical content from different subjects.
    let content_id = sqlx::query_scalar::<_, i64>(
        r"INSERT INTO schema_contents (schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint)
          VALUES ($1, $2, $3, $4, $5)
          ON CONFLICT (raw_fingerprint) DO UPDATE SET schema_type = EXCLUDED.schema_type
          RETURNING id",
    )
    .bind(schema.schema_type)
    .bind(schema.schema_text)
    .bind(schema.canonical_form)
    .bind(schema.fingerprint)
    .bind(schema.raw_fingerprint)
    .fetch_one(&mut *tx)
    .await?;

    // Create new version pointing to content.
    let version_num: i32 = sqlx::query_scalar(
        r"INSERT INTO schema_versions (subject_id, version, content_id)
          VALUES ($1, COALESCE((SELECT MAX(version) FROM schema_versions WHERE subject_id = $1), 0) + 1, $2)
          RETURNING version",
    )
    .bind(subject_id)
    .bind(content_id)
    .fetch_one(&mut *tx)
    .await?;

    // Store references only when provided and content has none yet.
    if !refs.is_empty() {
        let has_refs: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM schema_references WHERE content_id = $1)",
        )
        .bind(content_id)
        .fetch_one(&mut *tx)
        .await?;

        if !has_refs {
            for r in refs {
                sqlx::query(
                    "INSERT INTO schema_references (content_id, name, subject, version) VALUES ($1, $2, $3, $4)",
                )
                .bind(content_id)
                .bind(&r.name)
                .bind(&r.subject)
                .bind(r.version)
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok((content_id, version_num, true))
}

/// Run compatibility check inside a transaction.
///
/// For transitive mode (empty `versions`), re-fetches all versions inside the
/// transaction for consistency. For non-transitive, uses the pre-fetched versions.
async fn run_compat_check(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subject_id: i64,
    compat: CompatCheck,
) -> Result<(), KoraError> {
    // Transitive mode (empty `versions`) re-fetches all versions inside the
    // transaction for consistency; non-transitive uses the pre-fetched set.
    if compat.versions.is_empty() {
        let rows = sqlx::query(
            r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sv.deleted = false
               ORDER BY sv.version",
        )
        .bind(subject_id)
        .fetch_all(&mut **tx)
        .await?;
        let versions = rows.iter().map(row_to_schema_version).collect::<Vec<_>>();
        crate::storage::compat::evaluate(&versions, &compat)
    } else {
        crate::storage::compat::evaluate(&compat.versions, &compat)
    }
}

/// Check if a subject already has an active version with a given fingerprint.
///
/// Returns `(content_id, version)` if found.
async fn find_existing_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subject_id: i64,
    fingerprint: &str,
    fp_column: &str,
) -> Result<Option<(i64, i32)>, sqlx::Error> {
    // SAFETY (sqlx 0.9 `SqlSafeStr`): `fp_column` is one of two hardcoded
    // literals ("fingerprint" / "raw_fingerprint") chosen by the caller — never
    // user input. A column name cannot be a bind parameter, so it is
    // interpolated; the fingerprint value and subject_id are bound. The query
    // text contains no untrusted data, so `AssertSqlSafe` is sound.
    sqlx::query_as::<_, (i64, i32)>(sqlx::AssertSqlSafe(format!(
        r"SELECT sv.content_id, sv.version FROM schema_versions sv
              JOIN schema_contents sc ON sv.content_id = sc.id
              WHERE sv.subject_id = $1 AND sc.{fp_column} = $2 AND sv.deleted = false
              ORDER BY sv.version LIMIT 1"
    )))
    .bind(subject_id)
    .bind(fingerprint)
    .fetch_optional(&mut **tx)
    .await
}

/// Map a `schema_versions`-join row (aliasing `sub.name AS subject`) to a
/// [`SchemaVersion`]. Used by the in-transaction compatibility re-fetch.
fn row_to_schema_version(row: &sqlx::postgres::PgRow) -> SchemaVersion {
    SchemaVersion {
        subject: sqlx::Row::get(row, "subject"),
        id: sqlx::Row::get(row, "id"),
        version: sqlx::Row::get(row, "version"),
        schema: sqlx::Row::get(row, "schema_text"),
        schema_type: sqlx::Row::get(row, "schema_type"),
        references: Vec::new(),
    }
}
