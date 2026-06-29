//! Subject-related API handlers.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    response::IntoResponse,
};
use serde::Deserialize;

use crate::api::mode::enforce_writable;
use crate::error::KoraError;
use crate::schema::{self, SchemaFormat};
use crate::storage::{DynStorage, schemas, subjects};
use crate::types::SchemaReference;

// -- Types --

/// Request body for schema registration and check endpoints.
#[derive(Debug, Deserialize)]
pub struct SchemaRequest {
    /// The raw schema string (JSON-encoded).
    pub schema: String,
    /// Schema format — defaults to AVRO when absent.
    #[serde(rename = "schemaType")]
    pub schema_type: Option<String>,
    /// Optional schema references (for Protobuf imports, JSON Schema `$ref`, etc.).
    #[serde(default)]
    pub references: Option<Vec<SchemaReference>>,
}

/// Query parameters for `GET /subjects` with Confluent-specific filters.
#[derive(Debug, Deserialize)]
pub struct ListSubjectsParams {
    /// When true, include soft-deleted subjects in the response.
    #[serde(default)]
    pub deleted: bool,
    /// When true, return ONLY soft-deleted subjects (takes precedence over `deleted`).
    #[serde(default, rename = "deletedOnly")]
    pub deleted_only: bool,
    /// Accept-and-ignore: Confluent OSS param that overlaps with `deleted`/`deletedOnly`.
    #[serde(default, rename = "lookupDeletedSubject")]
    _lookup_deleted_subject: bool,
    /// Subject name prefix filter. Default `:*:` means match all.
    #[serde(default = "default_subject_prefix", rename = "subjectPrefix")]
    pub subject_prefix: String,
    /// Pagination offset (default 0).
    #[serde(default)]
    pub offset: i64,
    /// Pagination limit (-1 = unlimited, default).
    #[serde(default = "default_limit")]
    pub limit: i64,
}

pub(crate) fn default_subject_prefix() -> String {
    ":*:".to_string()
}

/// Query parameters for `GET /subjects/{subject}/versions` with Confluent-specific filters.
#[derive(Debug, Deserialize)]
pub struct ListVersionsParams {
    /// When true, include soft-deleted versions in the response.
    #[serde(default)]
    pub deleted: bool,
    /// When true, return ONLY soft-deleted versions (takes precedence over `deleted`).
    #[serde(default, rename = "deletedOnly")]
    pub deleted_only: bool,
    /// When true, soft-deleted versions appear as negative numbers.
    #[serde(default, rename = "deletedAsNegative")]
    pub deleted_as_negative: bool,
    /// Pagination offset (default 0).
    #[serde(default)]
    pub offset: i64,
    /// Pagination limit (-1 = unlimited, default).
    #[serde(default = "default_limit")]
    pub limit: i64,
}

/// Default limit: -1 (unlimited). Shared by all pagination param structs.
pub(crate) const fn default_limit() -> i64 {
    -1
}

/// Query parameters for schema registration (`POST /subjects/{subject}/versions`).
#[derive(Debug, Deserialize)]
pub struct RegisterParams {
    /// When true, normalize schema before fingerprint comparison (already default behavior).
    #[serde(default)]
    pub normalize: bool,
    /// Schema format hint (accept-and-ignore — format determined from body's `schemaType`).
    #[serde(default)]
    pub format: Option<String>,
}

/// Query parameters for schema check (`POST /subjects/{subject}`).
#[derive(Debug, Deserialize)]
pub struct CheckParams {
    /// When true, normalize schema before fingerprint comparison (already default behavior).
    #[serde(default)]
    pub normalize: bool,
    /// When true, include soft-deleted schemas in the lookup.
    #[serde(default)]
    pub deleted: bool,
    /// Schema format hint (accept-and-ignore).
    #[serde(default)]
    pub format: Option<String>,
}

/// Query parameters for `GET /subjects/{subject}/versions/{version}`.
#[derive(Debug, Deserialize)]
pub struct GetVersionParams {
    /// When true, include soft-deleted versions in the lookup.
    #[serde(default)]
    pub deleted: bool,
    /// Schema output format (accept-and-ignore — reference resolution not yet implemented).
    #[serde(default)]
    pub format: Option<String>,
    /// Reference output format (accept-and-ignore).
    #[serde(default, rename = "referenceFormat")]
    pub reference_format: Option<String>,
}

/// Query parameters for DELETE endpoints supporting `?permanent=true`.
#[derive(Debug, Deserialize)]
pub struct PermanentParams {
    /// When true, hard-delete (requires prior soft-delete).
    #[serde(default)]
    pub permanent: bool,
}

/// Query parameters for `GET /subjects/{subject}/versions/{version}/referencedby`.
#[derive(Debug, Deserialize)]
pub struct ReferencedByParams {
    /// When true, include soft-deleted referencing schema versions.
    #[serde(default)]
    pub deleted: bool,
    /// Pagination offset (default 0).
    #[serde(default)]
    pub offset: i64,
    /// Pagination limit (-1 = unlimited, default).
    #[serde(default = "default_limit")]
    pub limit: i64,
}

// -- Handlers --

/// Register a schema under a subject.
///
/// `POST /subjects/{subject}/versions`
///
/// # Errors
///
/// Returns `KoraError::InvalidSchema` (422) for unparseable schemas or
/// `KoraError::BackendDataStore` (500) for database failures.
pub async fn register_schema(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<RegisterParams>,
    body: Result<Json<SchemaRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let Json(body) = body.map_err(|e| KoraError::InvalidSchema(e.body_text()))?;

    validate_subject(&subject)?;

    // Enforce registry mode before expensive parsing/compat checks.
    enforce_writable(&storage, &subject).await?;

    let format = SchemaFormat::from_optional(body.schema_type.as_deref())?;
    let parsed = schema::parse(format, &body.schema)?;

    // Resolve effective normalize: explicit param OR subject/global config.
    let normalize = params.normalize || storage.get_effective_normalize(&subject).await?;

    // Build compatibility check to run inside the transaction (eliminates TOCTOU).
    let level = storage.get_effective_compatibility(&subject).await?;
    let direction = schema::CompatDirection::from_level(&level);
    let compat = if direction == schema::CompatDirection::None {
        None
    } else {
        let is_transitive = level.contains("TRANSITIVE");
        let versions = if is_transitive {
            // Transitive: pass empty vec — transaction will re-fetch for consistency.
            Vec::new()
        } else {
            // Non-transitive: pre-fetch latest (will be re-validated inside tx lock).
            storage
                .find_latest_schema_by_subject(&subject, false)
                .await?
                .into_iter()
                .collect()
        };
        Some(schemas::CompatCheck {
            versions,
            new_schema: body.schema.clone(),
            format,
            direction,
        })
    };

    // Validate references before any writes.
    let refs = body.references.as_deref().unwrap_or_default();
    if !refs.is_empty() {
        storage.validate_references(refs).await?;
    }

    let (id, version, _is_new) = storage
        .register_schema_atomically(
            &subject,
            &schemas::NewSchema {
                schema_type: format.as_str(),
                schema_text: &body.schema,
                canonical_form: &parsed.canonical_form,
                fingerprint: &parsed.fingerprint,
                raw_fingerprint: &parsed.raw_fingerprint,
            },
            refs,
            normalize,
            compat,
        )
        .await?;

    // Confluent RegisterSchemaResponse: id, version, schemaType, schema always present.
    // References included when non-empty (NON_EMPTY).
    let mut resp = serde_json::json!({
        "id": id,
        "version": version,
        "schemaType": format.as_str(),
        "schema": body.schema,
    });
    if !refs.is_empty() {
        resp["references"] = serde_json::json!(refs);
    }
    Ok(Json(resp))
}

/// Check if a schema is registered under a subject.
///
/// `POST /subjects/{subject}`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// or `KoraError::SchemaNotFound` (40403) if the schema is not registered.
pub async fn check_schema(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<CheckParams>,
    body: Result<Json<SchemaRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let Json(body) = body.map_err(|e| KoraError::InvalidSchema(e.body_text()))?;

    validate_subject(&subject)?;

    let format = SchemaFormat::from_optional(body.schema_type.as_deref())?;
    let parsed = schema::parse(format, &body.schema)?;

    // Resolve effective normalize: explicit param OR subject/global config.
    let normalize = params.normalize || storage.get_effective_normalize(&subject).await?;

    let fp = if normalize {
        &parsed.fingerprint
    } else {
        &parsed.raw_fingerprint
    };

    let subject_id = storage
        .find_subject_id_by_name(&subject, params.deleted)
        .await?
        .ok_or(KoraError::SubjectNotFound)?;

    let sv = storage
        .find_schema_by_subject_id_and_fingerprint(subject_id, fp, normalize, params.deleted)
        .await?
        .ok_or(KoraError::SchemaNotFound)?;

    Ok(Json(load_references(&storage, sv).await?))
}

/// List registered subjects.
///
/// `GET /subjects` — non-deleted subjects (default).
/// `GET /subjects?deleted=true` — all subjects (including soft-deleted).
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (500) for database failures.
pub async fn list_subjects(
    State(storage): State<DynStorage>,
    Query(params): Query<ListSubjectsParams>,
) -> Result<impl IntoResponse, KoraError> {
    let prefix = if params.subject_prefix == ":*:" || params.subject_prefix.is_empty() {
        None
    } else {
        Some(params.subject_prefix.as_str())
    };
    let names = storage
        .list_subjects(
            params.deleted,
            params.deleted_only,
            prefix,
            params.offset.max(0),
            params.limit,
        )
        .await?;
    Ok(Json(names))
}

/// List all versions of a subject.
///
/// `GET /subjects/{subject}/versions`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// or `KoraError::BackendDataStore` (500) for database failures.
pub async fn list_versions(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<ListVersionsParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;

    if !storage
        .subject_exists(&subject, params.deleted || params.deleted_only)
        .await?
    {
        return Err(KoraError::SubjectNotFound);
    }

    let versions = storage
        .list_schema_versions(
            &subject,
            params.deleted,
            params.deleted_only,
            params.deleted_as_negative,
            params.offset.max(0),
            params.limit,
        )
        .await?;
    Ok(Json(versions))
}

/// Retrieve a schema by subject and version.
///
/// `GET /subjects/{subject}/versions/{version}`
///
/// The version can be a number or "latest".
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// or `KoraError::VersionNotFound` (40402) if the version doesn't exist.
pub async fn get_schema_by_version(
    State(storage): State<DynStorage>,
    Path((subject, version)): Path<(String, String)>,
    Query(params): Query<GetVersionParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;

    let row = if version == "latest" {
        storage
            .find_latest_schema_by_subject(&subject, params.deleted)
            .await?
    } else {
        let v = parse_version(&version)?;
        storage
            .find_schema_by_subject_version(&subject, v, params.deleted)
            .await?
    };

    if let Some(sv) = row {
        Ok(Json(load_references(&storage, sv).await?))
    } else if storage.subject_exists(&subject, params.deleted).await? {
        Err(KoraError::VersionNotFound)
    } else {
        Err(KoraError::SubjectNotFound)
    }
}

/// Soft-delete a subject and all its versions.
///
/// `DELETE /subjects/{subject}`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist
/// (or isn't soft-deleted when `permanent=true`).
pub async fn delete_subject(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<PermanentParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;
    enforce_writable(&storage, &subject).await?;

    if params.permanent {
        // All checks (soft-deleted?, references?) run inside the transaction
        // to eliminate TOCTOU races with concurrent writers.
        match storage.hard_delete_subject(&subject).await? {
            subjects::HardDeleteResult::Deleted(versions) => Ok(Json(versions)),
            subjects::HardDeleteResult::NotSoftDeleted => {
                Err(KoraError::SubjectNotSoftDeleted(subject))
            }
            subjects::HardDeleteResult::NotFound => Err(KoraError::SubjectNotFound),
            subjects::HardDeleteResult::ReferenceExists(msg) => {
                Err(KoraError::ReferenceExists(msg))
            }
        }
    } else {
        if !storage.subject_exists(&subject, false).await? {
            return Err(subject_not_found_or_soft_deleted(&storage, &subject).await);
        }
        let versions = storage.soft_delete_subject(&subject).await?;
        Ok(Json(versions))
    }
}

/// Delete a single schema version (soft or hard).
///
/// `DELETE /subjects/{subject}/versions/{version}`
/// `DELETE /subjects/{subject}/versions/{version}?permanent=true`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) or `KoraError::VersionNotFound` (40402).
pub async fn delete_version(
    State(storage): State<DynStorage>,
    Path((subject, version)): Path<(String, String)>,
    Query(params): Query<PermanentParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;
    enforce_writable(&storage, &subject).await?;

    let deleted = if params.permanent {
        let v = parse_version(&version)?;
        // Subject must exist (even soft-deleted) for hard-delete to make sense.
        if !storage.subject_exists(&subject, true).await? {
            return Err(KoraError::SubjectNotFound);
        }
        // Confluent requires version to be soft-deleted first (40407).
        if storage.version_is_active(&subject, v).await? {
            return Err(KoraError::SchemaVersionNotSoftDeleted(subject, v));
        }
        if storage.is_version_referenced(&subject, v).await? {
            return Err(KoraError::ReferenceExists(format!("{subject} version {v}")));
        }
        storage.hard_delete_schema_version(&subject, v).await?
    } else {
        if !storage.subject_exists(&subject, false).await? {
            return Err(KoraError::SubjectNotFound);
        }
        if version == "latest" {
            storage.soft_delete_latest_schema(&subject).await?
        } else {
            let v = parse_version(&version)?;
            // Return 40406 if version is already soft-deleted.
            if storage.version_is_soft_deleted(&subject, v).await? {
                return Err(KoraError::SchemaVersionSoftDeleted(subject, v));
            }
            storage.soft_delete_schema_version(&subject, v).await?
        }
    }
    .ok_or(KoraError::VersionNotFound)?;

    Ok(Json(deleted))
}

/// Retrieve schema text by subject and version.
///
/// `GET /subjects/{subject}/versions/{version}/schema`
///
/// Returns the schema text only — no metadata wrapper.
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// or `KoraError::VersionNotFound` (40402) if the version doesn't exist.
pub async fn get_schema_text_by_version(
    State(storage): State<DynStorage>,
    Path((subject, version)): Path<(String, String)>,
    Query(params): Query<GetVersionParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;

    let row = if version == "latest" {
        storage
            .find_latest_schema_by_subject(&subject, params.deleted)
            .await?
    } else {
        let v = parse_version(&version)?;
        storage
            .find_schema_by_subject_version(&subject, v, params.deleted)
            .await?
    };

    if let Some(sv) = row {
        Ok(Json(sv.schema))
    } else if storage.subject_exists(&subject, params.deleted).await? {
        Err(KoraError::VersionNotFound)
    } else {
        Err(KoraError::SubjectNotFound)
    }
}

/// List schema IDs that reference a given subject/version.
///
/// `GET /subjects/{subject}/versions/{version}/referencedby`
///
/// Returns an array of global schema IDs (content IDs) whose schemas contain
/// a reference to the given subject at the given version.
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// `KoraError::VersionNotFound` (40402) if the version doesn't exist,
/// or `KoraError::InvalidVersion` (42202) for invalid version strings.
pub async fn get_referencing_ids_by_version(
    State(storage): State<DynStorage>,
    Path((subject, version)): Path<(String, String)>,
    Query(params): Query<ReferencedByParams>,
) -> Result<impl IntoResponse, KoraError> {
    validate_subject(&subject)?;
    let v = parse_version(&version)?;

    let ids = storage
        .find_referencing_schema_ids(
            &subject,
            v,
            params.deleted,
            params.offset.max(0),
            params.limit,
        )
        .await?;

    // Lazy existence check: only when result is empty (avoids extra queries on happy path).
    if ids.is_empty() {
        if !storage.subject_exists(&subject, false).await? {
            return Err(KoraError::SubjectNotFound);
        }
        if storage
            .find_schema_by_subject_version(&subject, v, false)
            .await?
            .is_none()
        {
            return Err(KoraError::VersionNotFound);
        }
    }

    Ok(Json(ids))
}

// -- Helpers --

/// Populate schema references on a `SchemaVersion` before returning to the client.
async fn load_references(
    storage: &DynStorage,
    mut sv: schemas::SchemaVersion,
) -> Result<schemas::SchemaVersion, KoraError> {
    sv.references = storage.find_references_by_schema_id(sv.id).await?;
    Ok(sv)
}

/// Return `SubjectNotFound` or `SubjectSoftDeleted` based on subject state.
async fn subject_not_found_or_soft_deleted(storage: &DynStorage, subject: &str) -> KoraError {
    if storage
        .subject_is_soft_deleted(subject)
        .await
        .unwrap_or(false)
    {
        KoraError::SubjectSoftDeleted(subject.to_string())
    } else {
        KoraError::SubjectNotFound
    }
}

/// Maximum allowed length for a subject name.
const MAX_SUBJECT_LENGTH: usize = 255;

/// Validate the subject path parameter.
fn validate_subject(subject: &str) -> Result<(), KoraError> {
    if subject.is_empty() {
        return Err(KoraError::InvalidSchema(
            "Subject name must not be empty".into(),
        ));
    }
    if subject.len() > MAX_SUBJECT_LENGTH {
        return Err(KoraError::InvalidSchema(
            "Subject name exceeds maximum length".into(),
        ));
    }
    if subject.contains('\0') {
        return Err(KoraError::InvalidSchema(
            "Subject name contains invalid characters".into(),
        ));
    }
    Ok(())
}

/// Parse a version string to a positive i32.
///
/// Confluent accepts only positive integers and `"latest"`. Everything else
/// (0, negatives, non-numeric strings) returns 42202 (`InvalidVersion`).
fn parse_version(version: &str) -> Result<i32, KoraError> {
    let v: i32 = version
        .parse()
        .map_err(|_| KoraError::InvalidVersion(version.to_string()))?;
    if v < 1 {
        return Err(KoraError::InvalidVersion(version.to_string()));
    }
    Ok(v)
}
