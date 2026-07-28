//! Confluent-compatible error types and response formatting.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

// -- Constants --

/// Content type for all Schema Registry responses.
pub const CONTENT_TYPE_SCHEMA_REGISTRY: &str = "application/vnd.schemaregistry.v1+json";

// -- Types --

/// Application-level errors mapped to Confluent Schema Registry error codes.
#[derive(Debug, thiserror::Error)]
pub enum KoraError {
    /// Invalid schema (42201).
    #[error("Invalid schema: {0}")]
    InvalidSchema(String),
    /// Subject not found (40401).
    #[error("Subject not found")]
    SubjectNotFound,
    /// Version not found (40402).
    #[error("Version not found")]
    VersionNotFound,
    /// Schema not found (40403).
    #[error("Schema not found")]
    SchemaNotFound,
    /// Schema reference not found (42201).
    #[error("Invalid schema: {0}")]
    ReferenceNotFound(String),
    /// Invalid compatibility level (42203).
    #[error("Invalid compatibility level: {0}")]
    InvalidCompatibilityLevel(String),
    /// Schema is referenced and cannot be deleted (42206).
    #[error("One or more references exist to the schema {0}")]
    ReferenceExists(String),
    /// Subject was soft-deleted (40404).
    #[error("Subject '{0}' was soft deleted. Set permanent=true to delete permanently")]
    SubjectSoftDeleted(String),
    /// Subject was NOT soft-deleted — hard-delete precondition (40405).
    #[error("Subject '{0}' was not deleted first before being permanently deleted")]
    SubjectNotSoftDeleted(String),
    /// Schema version was soft-deleted (40406).
    #[error("Subject '{0}' Version {1} was soft deleted. Set permanent=true to delete permanently")]
    SchemaVersionSoftDeleted(String, i32),
    /// Schema version was NOT soft-deleted — hard-delete precondition (40407).
    #[error("Subject '{0}' Version {1} was not deleted first before being permanently deleted")]
    SchemaVersionNotSoftDeleted(String, i32),
    /// Subject compatibility level not configured (40408).
    #[error("Subject '{0}' does not have subject-level compatibility configured")]
    SubjectCompatibilityNotConfigured(String),
    /// Subject mode not configured (40409).
    #[error("Subject '{0}' does not have subject-level mode configured")]
    SubjectModeNotConfigured(String),
    /// Incompatible schema (40901).
    #[error("Schema being registered is incompatible with an earlier schema")]
    IncompatibleSchema,
    /// Invalid version (42202).
    #[error("Invalid version: {0}")]
    InvalidVersion(String),
    /// Invalid mode (42204).
    #[error("Invalid mode: {0}")]
    InvalidMode(String),
    /// Operation not permitted (42205).
    #[error("Operation not permitted")]
    OperationNotPermitted,
    /// Backend data store error (50001).
    #[error("Error in the backend data store: {0}")]
    BackendDataStore(String),
    /// Operation timed out (50002).
    #[error("Operation timed out")]
    OperationTimeout,
    /// Error while forwarding request to primary (50003).
    #[error("Error while forwarding the request to the primary")]
    ForwardingError,
}

/// Confluent-compatible JSON error body.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error_code: u32,
    message: String,
}

// -- Impls --

impl From<sqlx::Error> for KoraError {
    fn from(err: sqlx::Error) -> Self {
        Self::BackendDataStore(err.to_string())
    }
}

impl KoraError {
    /// Confluent numeric error code.
    const fn error_code(&self) -> u32 {
        match self {
            Self::InvalidSchema(_) | Self::ReferenceNotFound(_) => 42201,
            Self::InvalidCompatibilityLevel(_) => 42203,
            Self::SubjectNotFound => 40401,
            Self::VersionNotFound => 40402,
            Self::SchemaNotFound => 40403,
            Self::SubjectSoftDeleted(_) => 40404,
            Self::SubjectNotSoftDeleted(_) => 40405,
            Self::SchemaVersionSoftDeleted(_, _) => 40406,
            Self::SchemaVersionNotSoftDeleted(_, _) => 40407,
            Self::SubjectCompatibilityNotConfigured(_) => 40408,
            Self::SubjectModeNotConfigured(_) => 40409,
            Self::IncompatibleSchema => 40901,
            Self::InvalidVersion(_) => 42202,
            Self::InvalidMode(_) => 42204,
            Self::OperationNotPermitted => 42205,
            Self::ReferenceExists(_) => 42206,
            Self::BackendDataStore(_) => 50001,
            Self::OperationTimeout => 50002,
            Self::ForwardingError => 50003,
        }
    }

    /// HTTP status code derived from the Confluent error code.
    const fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidSchema(_)
            | Self::ReferenceNotFound(_)
            | Self::InvalidCompatibilityLevel(_)
            | Self::InvalidVersion(_)
            | Self::InvalidMode(_)
            | Self::OperationNotPermitted
            | Self::ReferenceExists(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::SubjectNotFound
            | Self::VersionNotFound
            | Self::SchemaNotFound
            | Self::SubjectSoftDeleted(_)
            | Self::SubjectNotSoftDeleted(_)
            | Self::SchemaVersionSoftDeleted(_, _)
            | Self::SchemaVersionNotSoftDeleted(_, _)
            | Self::SubjectCompatibilityNotConfigured(_)
            | Self::SubjectModeNotConfigured(_) => StatusCode::NOT_FOUND,
            Self::IncompatibleSchema => StatusCode::CONFLICT,
            Self::BackendDataStore(_) | Self::OperationTimeout | Self::ForwardingError => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for KoraError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            error_code: self.error_code(),
            message: self.to_string(),
        };

        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error_code = body.error_code, error = %self, "internal server error");
        }

        (status, Json(body)).into_response()
    }
}
