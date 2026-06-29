//! Registry mode API handlers.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

use crate::api::compatibility::DefaultToGlobalParams;
use crate::error::KoraError;
use crate::storage::DynStorage;

// -- Types --

/// Valid registry modes matching the Confluent Schema Registry spec.
pub const VALID_MODES: &[&str] = &[
    "READWRITE",
    "READONLY",
    "READONLY_OVERRIDE",
    "IMPORT",
    "FORWARD",
];

/// Request body for mode updates.
#[derive(Debug, Deserialize)]
pub struct ModeRequest {
    /// The registry mode to set.
    pub mode: String,
}

/// Query parameters for `PUT /mode`.
#[derive(Debug, Deserialize)]
pub struct ModeSetParams {
    /// When true, force the mode change even if there are pending operations.
    #[serde(default)]
    pub force: bool,
}

/// Query parameters for `DELETE /mode/{subject}`.
#[derive(Debug, Deserialize)]
pub struct ModeDeleteParams {
    /// When true, propagate deletion to child subjects.
    #[serde(default)]
    pub recursive: bool,
}

// -- Global handlers --

/// Get the global registry mode.
///
/// `GET /mode`
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (500) for database failures.
pub async fn get_global_mode(
    State(storage): State<DynStorage>,
    Query(_params): Query<DefaultToGlobalParams>,
) -> Result<impl IntoResponse, KoraError> {
    let m = storage.get_global_mode().await?;
    Ok(Json(serde_json::json!({ "mode": m })))
}

/// Update the global registry mode.
///
/// `PUT /mode`
///
/// # Errors
///
/// Returns `KoraError::InvalidMode` (42204) for invalid modes.
pub async fn set_global_mode(
    State(storage): State<DynStorage>,
    Query(_params): Query<ModeSetParams>,
    Json(body): Json<ModeRequest>,
) -> Result<impl IntoResponse, KoraError> {
    validate_mode(&body.mode)?;
    let m = storage.set_global_mode(&body.mode).await?;
    Ok(Json(serde_json::json!({ "mode": m })))
}

/// Delete (reset) the global registry mode to READWRITE.
///
/// `DELETE /mode`
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (500) for database failures.
pub async fn delete_global_mode(
    State(storage): State<DynStorage>,
) -> Result<impl IntoResponse, KoraError> {
    let prev = storage.delete_global_mode().await?;
    Ok(Json(serde_json::json!({ "mode": prev })))
}

// -- Per-subject handlers --

/// Get the registry mode for a subject.
///
/// `GET /mode/{subject}`
///
/// # Errors
///
/// Returns `KoraError::SubjectModeNotConfigured` (40409) if no per-subject mode is set
/// and `defaultToGlobal` is not true.
pub async fn get_subject_mode(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<DefaultToGlobalParams>,
) -> Result<impl IntoResponse, KoraError> {
    if let Some(m) = storage.get_subject_mode(&subject).await? {
        return Ok(Json(serde_json::json!({ "mode": m })));
    }

    if params.default_to_global {
        let m = storage.get_global_mode().await?;
        Ok(Json(serde_json::json!({ "mode": m })))
    } else {
        Err(KoraError::SubjectModeNotConfigured(subject))
    }
}

/// Update the registry mode for a subject.
///
/// `PUT /mode/{subject}`
///
/// # Errors
///
/// Returns `KoraError::InvalidMode` (42204) for invalid modes.
pub async fn set_subject_mode(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(_params): Query<ModeSetParams>,
    Json(body): Json<ModeRequest>,
) -> Result<impl IntoResponse, KoraError> {
    validate_mode(&body.mode)?;
    let m = storage.set_subject_mode(&subject, &body.mode).await?;
    Ok(Json(serde_json::json!({ "mode": m })))
}

/// Delete per-subject registry mode (resets to READWRITE, falls back to global).
///
/// `DELETE /mode/{subject}`
///
/// When `recursive=true`, also clears mode on child subjects (prefix match) atomically.
///
/// # Errors
///
/// Returns `KoraError::SubjectNotFound` (40401) if no per-subject mode exists.
pub async fn delete_subject_mode(
    State(storage): State<DynStorage>,
    Path(subject): Path<String>,
    Query(params): Query<ModeDeleteParams>,
) -> Result<impl IntoResponse, KoraError> {
    let prev = if params.recursive {
        storage.delete_subject_mode_recursive(&subject).await?
    } else {
        storage.delete_subject_mode(&subject).await?
    };

    let prev = prev.ok_or(KoraError::SubjectNotFound)?;
    Ok(Json(serde_json::json!({ "mode": prev })))
}

// -- Helpers --

/// Modes that permit write operations (registration, deletion).
const WRITE_MODES: &[&str] = &["READWRITE", "IMPORT", "FORWARD", "READONLY_OVERRIDE"];

/// Validate that a mode string is one of the known values.
fn validate_mode(m: &str) -> Result<(), KoraError> {
    if VALID_MODES.contains(&m) {
        Ok(())
    } else {
        Err(KoraError::InvalidMode(m.to_string()))
    }
}

/// Check that the effective mode for a subject permits write operations.
///
/// Returns `Err(OperationNotPermitted)` if the mode blocks writes.
///
/// # Errors
///
/// Returns `KoraError::OperationNotPermitted` (42205) or a database error.
pub async fn enforce_writable(storage: &DynStorage, subject: &str) -> Result<(), KoraError> {
    let effective = storage.get_effective_mode(subject).await?;
    if WRITE_MODES.contains(&effective.as_str()) {
        Ok(())
    } else {
        Err(KoraError::OperationNotPermitted)
    }
}
