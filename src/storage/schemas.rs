//! Schema storage operations.
//!
//! Two-table design: `schema_contents` stores unique schema text (global dedup),
//! `schema_versions` maps (subject, version) to a content ID.

use sqlx::{PgPool, Row};

// -- Types --

/// Data needed to insert a new schema version.
pub struct NewSchema<'a> {
    /// Format identifier (e.g. "AVRO").
    pub schema_type: &'a str,
    /// Original schema text as submitted by the client.
    pub schema_text: &'a str,
    /// Canonical form used for deduplication.
    pub canonical_form: &'a str,
    /// Fingerprint of the canonical form (for normalized dedup).
    pub fingerprint: &'a str,
    /// Fingerprint of the raw schema text (for non-normalized dedup).
    pub raw_fingerprint: &'a str,
}

/// A subject-version pair, returned by schema ID cross-reference lookups.
#[derive(Debug, serde::Serialize)]
pub struct SubjectVersion {
    /// Subject name.
    pub subject: String,
    /// Version number within the subject.
    pub version: i32,
}

/// A schema with its subject context, returned by version lookups.
#[derive(Debug, serde::Serialize)]
pub struct SchemaVersion {
    /// Subject name.
    pub subject: String,
    /// Global schema ID (content ID, shared across subjects with identical content).
    pub id: i64,
    /// Version number within the subject.
    pub version: i32,
    /// Raw schema text.
    pub schema: String,
    /// Schema format (always included — Confluent serializes "AVRO" via `NON_EMPTY`).
    #[serde(rename = "schemaType")]
    pub schema_type: String,
    /// Schema references (Protobuf imports, JSON Schema `$ref`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<crate::types::SchemaReference>,
}

// -- Registration --

/// A compatibility check to run inside the registration transaction.
///
/// Populated by the handler with the schemas to check against.
/// Running inside the transaction (after locking the subject row)
/// guarantees no other registration can slip between the check and the insert.
pub struct CompatCheck {
    /// Schemas to check compatibility against (fetched before the transaction
    /// for the non-transitive case, or inside for transitive).
    pub versions: Vec<SchemaVersion>,
    /// The new schema text to check.
    pub new_schema: String,
    /// The format of the new schema.
    pub format: crate::schema::SchemaFormat,
    /// The compatibility direction resolved from the configured level.
    pub direction: crate::schema::CompatDirection,
}

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
///
/// # Errors
///
/// Returns a database error on connection or constraint failure, or
/// `KoraError::IncompatibleSchema` if the compatibility check fails.
pub async fn register_schema_atomically(
    pool: &PgPool,
    subject_name: &str,
    schema: &NewSchema<'_>,
    refs: &[crate::types::SchemaReference],
    normalize: bool,
    compat: Option<CompatCheck>,
) -> Result<(i64, i32, bool), crate::error::KoraError> {
    let mut tx = pool.begin().await?;

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
) -> Result<(), crate::error::KoraError> {
    let versions = if compat.versions.is_empty() {
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
        rows.iter().map(row_to_schema_version).collect::<Vec<_>>()
    } else {
        compat.versions
    };

    for existing in &versions {
        let Ok(existing_format) =
            crate::schema::SchemaFormat::from_optional(Some(&existing.schema_type))
        else {
            continue;
        };
        if existing_format != compat.format {
            continue;
        }
        let result = crate::schema::check_compatibility(
            compat.format,
            &compat.new_schema,
            &existing.schema,
            compat.direction,
        )?;
        if !result.is_compatible {
            return Err(crate::error::KoraError::IncompatibleSchema);
        }
    }
    Ok(())
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

// -- Lookups --

/// Fetch all active versions for a subject in a single query.
///
/// Replaces the N+1 pattern of listing version numbers then fetching each one
/// individually. Used by compatibility checks (transitive mode).
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_all_active_versions(
    pool: &PgPool,
    subject: &str,
) -> Result<Vec<SchemaVersion>, sqlx::Error> {
    let rows = sqlx::query(
        r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.deleted = false
           ORDER BY sv.version",
    )
    .bind(subject)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_schema_version).collect())
}

/// Find a schema by subject name and version number.
///
/// When `include_deleted` is true, soft-deleted versions are included in the lookup.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_schema_by_subject_version(
    pool: &PgPool,
    subject: &str,
    version: i32,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, sqlx::Error> {
    let query = if include_deleted {
        r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.version = $2"
    } else {
        r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = false"
    };
    sqlx::query(query)
        .bind(subject)
        .bind(version)
        .fetch_optional(pool)
        .await
        .map(|opt| opt.as_ref().map(row_to_schema_version))
}

/// Find the latest schema version for a subject.
///
/// When `include_deleted` is true, soft-deleted versions are considered when
/// finding the latest version.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_latest_schema_by_subject(
    pool: &PgPool,
    subject: &str,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, sqlx::Error> {
    let query = if include_deleted {
        r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1
           ORDER BY sv.version DESC LIMIT 1"
    } else {
        r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
           FROM schema_versions sv
           JOIN subjects sub ON sv.subject_id = sub.id
           JOIN schema_contents sc ON sv.content_id = sc.id
           WHERE sub.name = $1 AND sv.deleted = false
           ORDER BY sv.version DESC LIMIT 1"
    };
    sqlx::query(query)
        .bind(subject)
        .fetch_optional(pool)
        .await
        .map(|opt| opt.as_ref().map(row_to_schema_version))
}

/// Find a schema by subject ID and fingerprint (for check-if-registered).
///
/// When `normalize` is true, matches on canonical fingerprint; otherwise on raw fingerprint.
/// When `include_deleted` is true, soft-deleted versions are included in the lookup.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_schema_by_subject_id_and_fingerprint(
    pool: &PgPool,
    subject_id: i64,
    fingerprint: &str,
    normalize: bool,
    include_deleted: bool,
) -> Result<Option<SchemaVersion>, sqlx::Error> {
    let query = match (normalize, include_deleted) {
        (true, true) => {
            r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.fingerprint = $2"
        }
        (true, false) => {
            r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.fingerprint = $2 AND sv.deleted = false"
        }
        (false, true) => {
            r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.raw_fingerprint = $2"
        }
        (false, false) => {
            r"SELECT sc.id, sub.name as subject, sv.version, sc.schema_type, sc.schema_text
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE sv.subject_id = $1 AND sc.raw_fingerprint = $2 AND sv.deleted = false"
        }
    };
    sqlx::query(query)
        .bind(subject_id)
        .bind(fingerprint)
        .fetch_optional(pool)
        .await
        .map(|opt| opt.as_ref().map(row_to_schema_version))
}

/// Find a schema by its global content ID (ignores soft-delete — IDs are permanent).
/// Returns `(schema_text, schema_type)`.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_schema_by_id(
    pool: &PgPool,
    id: i64,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query("SELECT schema_text, schema_type FROM schema_contents WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map(|opt| {
            opt.as_ref()
                .map(|row| (row.get("schema_text"), row.get("schema_type")))
        })
}

/// Get the maximum schema content ID in the registry.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_max_schema_id(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM schema_contents")
        .fetch_one(pool)
        .await
        .map(|opt| opt.unwrap_or(0))
}

/// Check if a schema content exists by global ID (ignores soft-delete — IDs are permanent).
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn schema_exists(pool: &PgPool, id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM schema_contents WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
}

// -- Cross-references --

/// Find all subjects that use a given schema content ID, with pagination.
///
/// - `include_deleted`: when true, include soft-deleted subjects/versions.
/// - `subject_filter`: when `Some`, filter to a specific subject name.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_subjects_by_schema_id(
    pool: &PgPool,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    match (subject_filter, limit >= 0) {
        (Some(filter), true) => sqlx::query_scalar(
            r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name OFFSET $4 LIMIT $5",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(filter)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await,

        (Some(filter), false) => sqlx::query_scalar(
            r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name OFFSET $4",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(filter)
        .bind(offset)
        .fetch_all(pool)
        .await,

        (None, true) => sqlx::query_scalar(
            r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name OFFSET $3 LIMIT $4",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await,

        (None, false) => sqlx::query_scalar(
            r"SELECT DISTINCT sub.name FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name OFFSET $3",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(offset)
        .fetch_all(pool)
        .await,
    }
}

/// Find all subject-version pairs that use a given schema content ID, with pagination.
///
/// - `include_deleted`: when true, include soft-deleted subjects/versions.
/// - `subject_filter`: when `Some`, filter to a specific subject name.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn find_versions_by_schema_id(
    pool: &PgPool,
    id: i64,
    include_deleted: bool,
    subject_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SubjectVersion>, sqlx::Error> {
    let rows = match (subject_filter, limit >= 0) {
        (Some(filter), true) => sqlx::query(
            r"SELECT sub.name as subject, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name, sv.version OFFSET $4 LIMIT $5",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(filter)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await?,

        (Some(filter), false) => sqlx::query(
            r"SELECT sub.name as subject, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
                 AND sub.name = $3
               ORDER BY sub.name, sv.version OFFSET $4",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(filter)
        .bind(offset)
        .fetch_all(pool)
        .await?,

        (None, true) => sqlx::query(
            r"SELECT sub.name as subject, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name, sv.version OFFSET $3 LIMIT $4",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await?,

        (None, false) => sqlx::query(
            r"SELECT sub.name as subject, sv.version
               FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sv.content_id = $1 AND (sv.deleted = false OR $2) AND (sub.deleted = false OR $2)
               ORDER BY sub.name, sv.version OFFSET $3",
        )
        .bind(id)
        .bind(include_deleted)
        .bind(offset)
        .fetch_all(pool)
        .await?,
    };

    Ok(rows
        .iter()
        .map(|row| SubjectVersion {
            subject: row.get("subject"),
            version: row.get("version"),
        })
        .collect())
}

// -- Listing --

/// List version numbers for a subject, sorted ascending, with pagination.
///
/// - `include_deleted`: when true, include soft-deleted versions in results.
/// - `deleted_only`: when true, return ONLY soft-deleted versions (takes precedence).
/// - `deleted_as_negative`: when true (requires `include_deleted`), soft-deleted
///   versions appear as negative numbers (e.g., version 2 deleted → -2).
/// - `offset` defaults to 0, `limit` of -1 means unlimited.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn list_schema_versions(
    pool: &PgPool,
    subject: &str,
    include_deleted: bool,
    deleted_only: bool,
    deleted_as_negative: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<i32>, sqlx::Error> {
    if deleted_only && deleted_as_negative {
        if limit >= 0 {
            sqlx::query_scalar(
                r"SELECT -sv.version as version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1 AND sv.deleted = true
                   ORDER BY sv.version OFFSET $2 LIMIT $3",
            )
            .bind(subject)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        } else {
            sqlx::query_scalar(
                r"SELECT -sv.version as version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1 AND sv.deleted = true
                   ORDER BY sv.version OFFSET $2",
            )
            .bind(subject)
            .bind(offset)
            .fetch_all(pool)
            .await
        }
    } else if deleted_only {
        if limit >= 0 {
            sqlx::query_scalar(
                r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1 AND sv.deleted = true
                   ORDER BY sv.version OFFSET $2 LIMIT $3",
            )
            .bind(subject)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        } else {
            sqlx::query_scalar(
                r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1 AND sv.deleted = true
                   ORDER BY sv.version OFFSET $2",
            )
            .bind(subject)
            .bind(offset)
            .fetch_all(pool)
            .await
        }
    } else if deleted_as_negative && include_deleted {
        // Soft-deleted versions appear as negative numbers, ordered by absolute value.
        if limit >= 0 {
            sqlx::query_scalar(
                r"SELECT CASE WHEN sv.deleted THEN -sv.version ELSE sv.version END as version
                   FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1
                   ORDER BY abs(sv.version) OFFSET $2 LIMIT $3",
            )
            .bind(subject)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        } else {
            sqlx::query_scalar(
                r"SELECT CASE WHEN sv.deleted THEN -sv.version ELSE sv.version END as version
                   FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
                   WHERE sub.name = $1
                   ORDER BY abs(sv.version) OFFSET $2",
            )
            .bind(subject)
            .bind(offset)
            .fetch_all(pool)
            .await
        }
    } else if limit >= 0 {
        sqlx::query_scalar(
            r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1 AND (sv.deleted = false OR $2)
               ORDER BY sv.version OFFSET $3 LIMIT $4",
        )
        .bind(subject)
        .bind(include_deleted)
        .bind(offset)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_scalar(
            r"SELECT sv.version FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
               WHERE sub.name = $1 AND (sv.deleted = false OR $2)
               ORDER BY sv.version OFFSET $3",
        )
        .bind(subject)
        .bind(include_deleted)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
}

/// List schemas across all subjects, with optional filtering and pagination.
///
/// - `include_deleted`: when true, include soft-deleted versions/subjects.
/// - `latest_only`: when true, return only the highest version per subject.
/// - `prefix`: when `Some`, filter to subjects whose name starts with this prefix.
/// - `offset` defaults to 0, `limit` of -1 means unlimited.
///
/// References are NOT populated — caller must load them separately.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn list_schemas(
    pool: &PgPool,
    include_deleted: bool,
    latest_only: bool,
    prefix: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<SchemaVersion>, sqlx::Error> {
    let like_pattern = prefix.filter(|p| !p.is_empty()).map(|p| {
        let escaped = p
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("{escaped}%")
    });

    let rows = if latest_only {
        list_schemas_latest(
            pool,
            include_deleted,
            like_pattern.as_deref(),
            offset,
            limit,
        )
        .await?
    } else {
        list_schemas_all_versions(
            pool,
            include_deleted,
            like_pattern.as_deref(),
            offset,
            limit,
        )
        .await?
    };

    Ok(rows.iter().map(row_to_schema_version).collect())
}

/// List latest version per subject (helper for `DISTINCT ON` variant).
async fn list_schemas_latest(
    pool: &PgPool,
    include_deleted: bool,
    like_pattern: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    match (like_pattern, limit >= 0) {
        (Some(pat), true) => {
            sqlx::query(
                r"SELECT DISTINCT ON (sub.name) sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version DESC
               OFFSET $3 LIMIT $4",
            )
            .bind(include_deleted)
            .bind(pat)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        }

        (Some(pat), false) => {
            sqlx::query(
                r"SELECT DISTINCT ON (sub.name) sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version DESC
               OFFSET $3",
            )
            .bind(include_deleted)
            .bind(pat)
            .bind(offset)
            .fetch_all(pool)
            .await
        }

        (None, true) => {
            sqlx::query(
                r"SELECT DISTINCT ON (sub.name) sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version DESC
               OFFSET $2 LIMIT $3",
            )
            .bind(include_deleted)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        }

        (None, false) => {
            sqlx::query(
                r"SELECT DISTINCT ON (sub.name) sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version DESC
               OFFSET $2",
            )
            .bind(include_deleted)
            .bind(offset)
            .fetch_all(pool)
            .await
        }
    }
}

/// List schema versions across all subjects (helper for non-`DISTINCT ON` variant).
async fn list_schemas_all_versions(
    pool: &PgPool,
    include_deleted: bool,
    like_pattern: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    match (like_pattern, limit >= 0) {
        (Some(pat), true) => {
            sqlx::query(
                r"SELECT sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version
               OFFSET $3 LIMIT $4",
            )
            .bind(include_deleted)
            .bind(pat)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        }

        (Some(pat), false) => {
            sqlx::query(
                r"SELECT sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
                 AND sub.name LIKE $2 ESCAPE '\'
               ORDER BY sub.name, sv.version
               OFFSET $3",
            )
            .bind(include_deleted)
            .bind(pat)
            .bind(offset)
            .fetch_all(pool)
            .await
        }

        (None, true) => {
            sqlx::query(
                r"SELECT sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version
               OFFSET $2 LIMIT $3",
            )
            .bind(include_deleted)
            .bind(offset)
            .bind(limit)
            .fetch_all(pool)
            .await
        }

        (None, false) => {
            sqlx::query(
                r"SELECT sub.name AS subject, sc.id, sv.version,
                     sc.schema_text, sc.schema_type
               FROM schema_versions sv
               JOIN subjects sub ON sv.subject_id = sub.id
               JOIN schema_contents sc ON sv.content_id = sc.id
               WHERE (sv.deleted = false OR $1) AND (sub.deleted = false OR $1)
               ORDER BY sub.name, sv.version
               OFFSET $2",
            )
            .bind(include_deleted)
            .bind(offset)
            .fetch_all(pool)
            .await
        }
    }
}

// -- Deletion --

/// Soft-delete the latest schema version for a subject. Returns the version number if found.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn soft_delete_latest_schema(
    pool: &PgPool,
    subject: &str,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(
        r"UPDATE schema_versions SET deleted = true
           WHERE id = (
             SELECT sv.id FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
             WHERE sub.name = $1 AND sv.deleted = false
             ORDER BY sv.version DESC LIMIT 1
           )
           RETURNING version",
    )
    .bind(subject)
    .fetch_optional(pool)
    .await
}

/// Soft-delete a single schema version. Returns the version number if found.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn soft_delete_schema_version(
    pool: &PgPool,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(
        r"UPDATE schema_versions SET deleted = true
           WHERE subject_id = (SELECT id FROM subjects WHERE name = $1)
             AND version = $2 AND deleted = false
           RETURNING version",
    )
    .bind(subject)
    .bind(version)
    .fetch_optional(pool)
    .await
}

/// Hard-delete a soft-deleted schema version. Returns the version number if found.
///
/// Only operates on rows where `deleted = true`. Content is never deleted
/// (global IDs are permanent).
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn hard_delete_schema_version(
    pool: &PgPool,
    subject: &str,
    version: i32,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(
        r"DELETE FROM schema_versions
           WHERE subject_id = (SELECT id FROM subjects WHERE name = $1)
             AND version = $2 AND deleted = true
           RETURNING version",
    )
    .bind(subject)
    .bind(version)
    .fetch_optional(pool)
    .await
}

// -- Status checks --

/// Check if a specific version is soft-deleted under a subject.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn version_is_soft_deleted(
    pool: &PgPool,
    subject: &str,
    version: i32,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r"SELECT EXISTS(
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
            WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = true
        )",
    )
    .bind(subject)
    .bind(version)
    .fetch_one(pool)
    .await
}

/// Check if a specific version is active (not soft-deleted) under a subject.
///
/// # Errors
///
/// Returns a database error on connection failure.
pub async fn version_is_active(
    pool: &PgPool,
    subject: &str,
    version: i32,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r"SELECT EXISTS(
            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id
            WHERE sub.name = $1 AND sv.version = $2 AND sv.deleted = false
        )",
    )
    .bind(subject)
    .bind(version)
    .fetch_one(pool)
    .await
}

// -- Helpers --

fn row_to_schema_version(row: &sqlx::postgres::PgRow) -> SchemaVersion {
    SchemaVersion {
        subject: row.get("subject"),
        id: row.get("id"),
        version: row.get("version"),
        schema: row.get("schema_text"),
        schema_type: row.get("schema_type"),
        references: Vec::new(),
    }
}
