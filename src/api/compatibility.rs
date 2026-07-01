//! Compatibility configuration and testing API handlers.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    response::IntoResponse,
};
use serde::Deserialize;

use crate::error::KoraError;
use crate::schema::{self, SchemaFormat};
use crate::storage::{DynStorage, types};
use crate::types::SchemaReference;

// -- Types --

/// Valid compatibility levels matching the Confluent Schema Registry spec.
pub const COMPATIBILITY_LEVELS: &[&str] = &[
    "BACKWARD",
    "BACKWARD_TRANSITIVE",
    "FORWARD",
    "FORWARD_TRANSITIVE",
    "FULL",
    "FULL_TRANSITIVE",
    "NONE",
];

/// Request body for compatibility config updates.
#[derive(Debug, Deserialize)]
pub struct CompatibilityRequest {
    /// The compatibility level to set.
    pub compatibility: String,
    /// When true, normalize schemas before comparison (persisted per-subject or globally).
    #[serde(default)]
    pub normalize: bool,
}

/// Query parameter for `defaultToGlobal` on config GET endpoints.
#[derive(Debug, Deserialize)]
pub struct DefaultToGlobalParams {
    /// When true, fall back to global config if subject has no per-subject config.
    #[serde(default, rename = "defaultToGlobal")]
    pub default_to_global: bool,
}

/// Request body for compatibility test endpoints.
#[derive(Debug, Deserialize)]
pub struct CompatibilityTestRequest {
    /// The new schema to test.
    pub schema: String,
    /// Schema format — defaults to AVRO when absent.
    #[serde(rename = "schemaType")]
    pub schema_type: Option<String>,
    /// Optional schema references.
    #[serde(default)]
    pub references: Option<Vec<SchemaReference>>,
}

/// Query parameters for compatibility test endpoints.
#[derive(Debug, Deserialize)]
pub struct CompatibilityTestParams {
    /// When true, return detailed incompatibility messages.
    #[serde(default)]
    pub verbose: bool,
    /// When true, normalize schemas before testing.
    #[serde(default)]
    pub normalize: bool,
}

// -- Config handlers --

/// Get the global compatibility configuration.
///
/// `GET /config`
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (500) for database failures.
pub async fn get_global_compatibility(
    State(storage): State<DynStorage>,
    Query(_params): Query<DefaultToGlobalParams>,
) -> Result<impl IntoResponse, KoraError> {
    let level = storage.get_global_level().await?;
    let normalize = storage.get_global_normalize().await?;
    Ok(Json(
        serde_json::json!({ "compatibilityLevel": level, "normalize": normalize }),
    ))
}

/// Update the global compatibility configuration.
///
/// `PUT /config`
///
/// # Errors
///
/// Returns `KoraError::InvalidCompatibilityLevel` (42203) for invalid levels.
pub async fn set_global_compatibility(
    State(storage): State<DynStorage>,
    Json(body): Json<CompatibilityRequest>,
) -> Result<impl IntoResponse, KoraError> {
    validate_level(&body.compatibility)?;
    let level = storage
        .set_global_level(&body.compatibility, body.normalize)
        .await?;
    Ok(Json(
        serde_json::json!({ "compatibility": level, "normalize": body.normalize }),
    ))
}

/// Get the compatibility configuration for a subject.
///
/// `GET /config/{subject}`
///
/// Returns the per-subject level if set, otherwise the global fallback.
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist.
pub async fn get_subject_compatibility(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<DefaultToGlobalParams>,
) -> Result<impl IntoResponse, KoraError> {
    // Confluent does not check subject existence — only config existence.
    if let Some(level) = storage.get_subject_level(&subject).await? {
        let normalize = storage
            .get_subject_normalize(&subject)
            .await?
            .unwrap_or(false);
        return Ok(Json(
            serde_json::json!({ "compatibilityLevel": level, "normalize": normalize }),
        ));
    }

    // No per-subject config — fall back or return 40408.
    if params.default_to_global {
        let level = storage.get_global_level().await?;
        let normalize = storage.get_global_normalize().await?;
        Ok(Json(
            serde_json::json!({ "compatibilityLevel": level, "normalize": normalize }),
        ))
    } else {
        Err(KoraError::SubjectCompatibilityNotConfigured(subject))
    }
}

/// Update the compatibility configuration for a subject.
///
/// `PUT /config/{subject}`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// or `KoraError::InvalidCompatibilityLevel` (42203) for invalid levels.
pub async fn set_subject_compatibility(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Json(body): Json<CompatibilityRequest>,
) -> Result<impl IntoResponse, KoraError> {
    // Confluent allows setting config on any subject name (no existence check).
    validate_level(&body.compatibility)?;
    let level = storage
        .set_subject_level(&subject, &body.compatibility, body.normalize)
        .await?;
    Ok(Json(
        serde_json::json!({ "compatibility": level, "normalize": body.normalize }),
    ))
}

/// Delete global compatibility configuration (reset to BACKWARD).
///
/// `DELETE /config`
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (50001) for database failures.
pub async fn delete_global_compatibility(
    State(storage): State<DynStorage>,
) -> Result<impl IntoResponse, KoraError> {
    let (prev_level, prev_normalize) = storage.delete_global_level().await?;
    // Confluent returns the previous Config object (before deletion).
    Ok(Json(
        serde_json::json!({ "compatibilityLevel": prev_level, "normalize": prev_normalize }),
    ))
}

/// Delete per-subject compatibility configuration.
///
/// `DELETE /config/{subject}`
///
/// Returns the **previous** per-subject level (not the global fallback).
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist.
pub async fn delete_subject_compatibility(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
) -> Result<impl IntoResponse, KoraError> {
    // Confluent checks config existence, not subject existence.
    let (prev_level, prev_normalize) = storage
        .delete_subject_level(&subject)
        .await?
        .ok_or(KoraError::SubjectNotFound)?;
    // Confluent returns the previous Config object (before deletion).
    Ok(Json(
        serde_json::json!({ "compatibilityLevel": prev_level, "normalize": prev_normalize }),
    ))
}

// -- Test handlers --

/// Test schema compatibility against a specific version.
///
/// `POST /compatibility/subjects/{subject}/versions/{version}`
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if the subject doesn't exist,
/// `KoraError::VersionNotFound` (40402) if the version doesn't exist,
/// or `KoraError::InvalidSchema` (42201) for unparseable schemas.
pub async fn test_compatibility_by_version(
    State(storage): State<DynStorage>,
    Path((subject, version)): Path<(String, String)>,
    Query(params): Query<CompatibilityTestParams>,
    body: Result<Json<CompatibilityTestRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let (format, body) = parse_compat_request(body)?;
    let existing = resolve_version(&storage, &subject, &version).await?;
    check_type_match(format, &existing)?;

    let level = storage.get_effective_compatibility(&subject).await?;
    let direction = schema::CompatDirection::from_level(&level);
    let result = schema::check_compatibility_async(
        format,
        body.schema.clone(),
        existing.schema.clone(),
        direction,
    )
    .await?;

    Ok(Json(compat_response(
        result.is_compatible,
        &result.messages,
        params.verbose,
    )))
}

/// Test schema compatibility against all versions of a subject.
///
/// `POST /compatibility/subjects/{subject}/versions`
///
/// # Errors
///
/// Returns `KoraError::InvalidSchema` (42201) for unparseable schemas.
pub async fn test_compatibility_against_all_versions(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<CompatibilityTestParams>,
    body: Result<Json<CompatibilityTestRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let (format, body) = parse_compat_request(body)?;

    // Confluent: nonexistent subject → is_compatible: true (no versions to check).
    let all_versions = storage.find_all_active_versions(&subject).await?;
    if all_versions.is_empty() {
        return Ok(Json(compat_response(true, &[], params.verbose)));
    }

    let level = storage.get_effective_compatibility(&subject).await?;
    let direction = schema::CompatDirection::from_level(&level);
    let mut all_messages = Vec::new();
    let mut is_compatible = true;

    for existing in &all_versions {
        // Skip type-mismatched versions (subject may have mixed types under NONE then switched).
        let Ok(existing_format) = SchemaFormat::from_optional(Some(&existing.schema_type)) else {
            continue;
        };
        if existing_format != format {
            continue;
        }

        let result = schema::check_compatibility_async(
            format,
            body.schema.clone(),
            existing.schema.clone(),
            direction,
        )
        .await?;
        if !result.is_compatible {
            is_compatible = false;
        }
        all_messages.extend(result.messages);
    }

    Ok(Json(compat_response(
        is_compatible,
        &all_messages,
        params.verbose,
    )))
}

// -- Helpers --

/// Parse the request body and validate the schema format.
fn parse_compat_request(
    body: Result<Json<CompatibilityTestRequest>, JsonRejection>,
) -> Result<(SchemaFormat, CompatibilityTestRequest), KoraError> {
    let Json(body) = body.map_err(|e| KoraError::InvalidSchema(e.body_text()))?;
    let format = SchemaFormat::from_optional(body.schema_type.as_deref())?;
    schema::parse(format, &body.schema)?;
    Ok((format, body))
}

/// Verify that the new schema type matches the existing one.
fn check_type_match(
    format: SchemaFormat,
    existing: &types::SchemaVersion,
) -> Result<(), KoraError> {
    let existing_format = SchemaFormat::from_optional(Some(&existing.schema_type))?;
    if format != existing_format {
        return Err(KoraError::InvalidSchema(format!(
            "Schema type mismatch: new is {} but existing is {}",
            format.as_str(),
            existing_format.as_str()
        )));
    }
    Ok(())
}

/// Build the compatibility response JSON, including `messages` only when verbose.
fn compat_response(is_compatible: bool, messages: &[String], verbose: bool) -> serde_json::Value {
    let mut body = serde_json::json!({"is_compatible": is_compatible});
    if verbose {
        body["messages"] = serde_json::json!(messages);
    }
    body
}

/// Resolve a version string ("latest" or positive integer) to a `SchemaVersion`.
async fn resolve_version(
    storage: &DynStorage,
    subject: &str,
    version: &str,
) -> Result<types::SchemaVersion, KoraError> {
    let row = if version == "latest" {
        storage
            .find_latest_schema_by_subject(subject, false)
            .await?
    } else {
        let v: i32 = version
            .parse()
            .map_err(|_| KoraError::InvalidVersion(version.to_string()))?;
        if v < 1 {
            return Err(KoraError::InvalidVersion(version.to_string()));
        }
        storage
            .find_schema_by_subject_version(subject, v, false)
            .await?
    };

    if let Some(sv) = row {
        Ok(sv)
    } else if storage.subject_exists(subject, false).await? {
        Err(KoraError::VersionNotFound)
    } else {
        Err(KoraError::SubjectNotFound)
    }
}

/// Validate that a compatibility level string is one of the known values.
fn validate_level(level: &str) -> Result<(), KoraError> {
    if COMPATIBILITY_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err(KoraError::InvalidCompatibilityLevel(level.to_string()))
    }
}
