//! Storage layer.
//!
//! The HTTP layer talks to the database exclusively through the [`Storage`]
//! trait, never to a concrete pool. [`PgStorage`] is the `PostgreSQL`
//! implementation — the only backing store. The wire API stays
//! Confluent-compatible regardless.
//!
//! All trait methods return [`KoraError`] so handlers stay backend-agnostic; a
//! driver error becomes [`KoraError::BackendDataStore`].

pub mod backends;
pub mod compat;
pub mod sql;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::KoraConfig;
use crate::error::KoraError;
use crate::types::SchemaReference;
use types::{CompatCheck, HardDeleteResult, NewSchema, SchemaVersion, SubjectVersion};

pub use backends::PgStorage;

// -- Shared handle --

/// A reference-counted, dynamically-dispatched storage backend. Cheap to clone
/// and shared across all request handlers via axum state.
pub type DynStorage = Arc<dyn Storage>;

/// Snapshot of connection-pool occupancy, surfaced as Prometheus gauges.
#[derive(Debug, Clone, Copy)]
pub struct PoolStats {
    /// Connections currently open (established) in the pool.
    pub size: u32,
    /// Connections currently idle (open but not executing a query).
    pub idle: u32,
}

// -- Errors --

/// Failure while establishing the storage backend at startup.
#[derive(Debug, thiserror::Error)]
pub enum StorageInitError {
    /// The database could not be reached or initialised.
    #[error("failed to initialise storage backend: {0}")]
    Backend(String),
}

// -- Trait --

/// Backend-agnostic persistence operations backing the Schema Registry API.
///
/// Implementors own their own connection pool and translate these operations to
/// dialect-correct SQL. Methods returning `Option` follow "row absent → `None`"
/// semantics; methods returning collections return them already ordered as the
/// API expects.
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // -- Lifecycle --

    /// Run pending schema migrations. Idempotent and safe to call on every boot.
    ///
    /// # Errors
    ///
    /// Returns an error if migrations fail.
    async fn migrate(&self) -> Result<(), KoraError>;

    /// Liveness probe — succeeds when the database answers a trivial query.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is unreachable.
    async fn ping(&self) -> Result<(), KoraError>;

    /// Number of unique schema contents in the registry (for the metrics gauge).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn schema_count(&self) -> Result<i64, KoraError>;

    /// Current connection-pool occupancy for the metrics gauges.
    fn pool_stats(&self) -> PoolStats;

    /// Apply declarative startup configuration. Idempotent and safe on every boot.
    ///
    /// When `default_compatibility` is set, reconciles the global compatibility
    /// level (`subject IS NULL`) to it and returns the applied value; otherwise a
    /// no-op returning `None`. Per-subject overrides are never touched.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn apply_startup_config(
        &self,
        default_compatibility: Option<&str>,
    ) -> Result<Option<String>, KoraError> {
        match default_compatibility {
            Some(level) => Ok(Some(self.reconcile_global_level(level).await?)),
            None => Ok(None),
        }
    }

    // -- Subjects --

    /// List subject names, sorted alphabetically, with pagination.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn list_subjects(
        &self,
        include_deleted: bool,
        deleted_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError>;

    /// Soft-delete a subject and all its versions; returns the deleted version
    /// numbers sorted ascending.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection or transaction failure.
    async fn soft_delete_subject(&self, name: &str) -> Result<Vec<i32>, KoraError>;

    /// Hard-delete a subject atomically after verifying preconditions.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection or transaction failure.
    async fn hard_delete_subject(&self, name: &str) -> Result<HardDeleteResult, KoraError>;

    /// Find a subject's ID by name, optionally including soft-deleted subjects.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_subject_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>, KoraError>;

    /// Check if a subject exists, optionally including soft-deleted subjects.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn subject_exists(&self, name: &str, include_deleted: bool) -> Result<bool, KoraError>;

    /// Check if a subject exists and is soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn subject_is_soft_deleted(&self, name: &str) -> Result<bool, KoraError>;

    // -- Schemas --

    /// Register a schema atomically: upsert subject, deduplicate content, create
    /// version, store references, and run the compatibility check inside the
    /// transaction. Returns `(content_id, version, is_new)`.
    ///
    /// # Errors
    ///
    /// Returns a database error, or [`KoraError::IncompatibleSchema`] when the
    /// in-transaction compatibility check fails.
    async fn register_schema_atomically(
        &self,
        subject_name: &str,
        schema: &NewSchema<'_>,
        refs: &[SchemaReference],
        normalize: bool,
        compat: Option<CompatCheck>,
    ) -> Result<(i64, i32, bool), KoraError>;

    /// Fetch all active versions for a subject in a single query.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_all_active_versions(
        &self,
        subject: &str,
    ) -> Result<Vec<SchemaVersion>, KoraError>;

    /// Find a schema by subject name and version number.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_schema_by_subject_version(
        &self,
        subject: &str,
        version: i32,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError>;

    /// Find the latest schema version for a subject.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_latest_schema_by_subject(
        &self,
        subject: &str,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError>;

    /// Find a schema by subject ID and fingerprint (for check-if-registered).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_schema_by_subject_id_and_fingerprint(
        &self,
        subject_id: i64,
        fingerprint: &str,
        normalize: bool,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError>;

    /// Find a schema by its global content ID (ignores soft-delete). Returns
    /// `(schema_text, schema_type)`.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_schema_by_id(&self, id: i64) -> Result<Option<(String, String)>, KoraError>;

    /// Get the maximum schema content ID in the registry.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_max_schema_id(&self) -> Result<i64, KoraError>;

    /// Check if a schema content exists by global ID (ignores soft-delete).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn schema_exists(&self, id: i64) -> Result<bool, KoraError>;

    /// Find all subjects that use a given schema content ID, with pagination.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_subjects_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError>;

    /// Find all subject-version pairs that use a given schema content ID.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_versions_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SubjectVersion>, KoraError>;

    /// List version numbers for a subject, sorted ascending, with pagination.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn list_schema_versions(
        &self,
        subject: &str,
        include_deleted: bool,
        deleted_only: bool,
        deleted_as_negative: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i32>, KoraError>;

    /// List schemas across all subjects, with optional filtering and pagination.
    /// References are not populated — the caller loads them separately.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn list_schemas(
        &self,
        include_deleted: bool,
        latest_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SchemaVersion>, KoraError>;

    /// Soft-delete the latest schema version for a subject. Returns the version.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn soft_delete_latest_schema(&self, subject: &str) -> Result<Option<i32>, KoraError>;

    /// Soft-delete a single schema version. Returns the version if found.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn soft_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError>;

    /// Hard-delete a soft-deleted schema version. Returns the version if found.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn hard_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError>;

    /// Check if a specific version is soft-deleted under a subject.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn version_is_soft_deleted(&self, subject: &str, version: i32)
    -> Result<bool, KoraError>;

    /// Check if a specific version is active (not soft-deleted) under a subject.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn version_is_active(&self, subject: &str, version: i32) -> Result<bool, KoraError>;

    // -- Compatibility config --

    /// Get the per-subject compatibility level only (no fallback).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_subject_level(&self, subject: &str) -> Result<Option<String>, KoraError>;

    /// Get the global compatibility level.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_global_level(&self) -> Result<String, KoraError>;

    /// Update the global compatibility level and return the new value.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn set_global_level(&self, level: &str, normalize: bool) -> Result<String, KoraError>;

    /// Reconcile the global compatibility level to `level`, returning the new
    /// value. Leaves `normalize` and per-subject overrides untouched.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn reconcile_global_level(&self, level: &str) -> Result<String, KoraError>;

    /// Set the per-subject compatibility level (upsert). Returns the new value.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn set_subject_level(
        &self,
        subject: &str,
        level: &str,
        normalize: bool,
    ) -> Result<String, KoraError>;

    /// Delete per-subject compatibility config. Returns the previous
    /// `(level, normalize)`, or `None` if not configured.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn delete_subject_level(
        &self,
        subject: &str,
    ) -> Result<Option<(String, bool)>, KoraError>;

    /// Get the global normalize setting.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_global_normalize(&self) -> Result<bool, KoraError>;

    /// Get the subject-level normalize setting (no fallback).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_subject_normalize(&self, subject: &str) -> Result<Option<bool>, KoraError>;

    /// Get the effective normalize setting for a subject (subject-level, then
    /// global fallback).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_effective_normalize(&self, subject: &str) -> Result<bool, KoraError> {
        if let Some(n) = self.get_subject_normalize(subject).await? {
            return Ok(n);
        }
        self.get_global_normalize().await
    }

    /// Get the effective compatibility level for a subject (subject-level, then
    /// global fallback).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_effective_compatibility(&self, subject: &str) -> Result<String, KoraError> {
        if let Some(level) = self.get_subject_level(subject).await? {
            return Ok(level);
        }
        self.get_global_level().await
    }

    /// Reset the global compatibility level to `BACKWARD`. Returns the previous
    /// `(compatibility_level, normalize)`.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn delete_global_level(&self) -> Result<(String, bool), KoraError>;

    // -- Mode --

    /// Get the global registry mode.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_global_mode(&self) -> Result<String, KoraError>;

    /// Update the global registry mode and return the new value.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn set_global_mode(&self, mode: &str) -> Result<String, KoraError>;

    /// Reset the global registry mode to `READWRITE`. Returns the previous mode.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn delete_global_mode(&self) -> Result<String, KoraError>;

    /// Get the per-subject registry mode only (no fallback).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError>;

    /// Set the per-subject registry mode (upsert). Returns the new value.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn set_subject_mode(&self, subject: &str, mode: &str) -> Result<String, KoraError>;

    /// Delete per-subject mode. Returns the previous mode, or `None`.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn delete_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError>;

    /// Delete per-subject mode for a subject and all child subjects (prefix
    /// match), atomically. Returns the parent's previous mode, or `None`.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn delete_subject_mode_recursive(
        &self,
        subject: &str,
    ) -> Result<Option<String>, KoraError>;

    /// Get the effective registry mode for a subject (subject-level, then global
    /// fallback) in a single query.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn get_effective_mode(&self, subject: &str) -> Result<String, KoraError>;

    // -- References --

    /// Validate that all referenced schemas exist and are not soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns [`KoraError::ReferenceNotFound`] if any referenced subject/version
    /// does not exist or is soft-deleted.
    async fn validate_references(&self, refs: &[SchemaReference]) -> Result<(), KoraError>;

    /// Find all references for a given schema content ID.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_references_by_schema_id(
        &self,
        content_id: i64,
    ) -> Result<Vec<SchemaReference>, KoraError>;

    /// Find references for many content IDs in a single query, returned as
    /// `(content_id, reference)` pairs. Used by listings to avoid an N+1 (one
    /// reference query per listed schema).
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_references_for_schema_ids(
        &self,
        content_ids: &[i64],
    ) -> Result<Vec<(i64, SchemaReference)>, KoraError>;

    /// Find content IDs of schemas that reference the given subject/version.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn find_referencing_schema_ids(
        &self,
        target_subject: &str,
        target_version: i32,
        include_deleted: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, KoraError>;

    /// Check if a subject/version is referenced by any active schema version.
    ///
    /// # Errors
    ///
    /// Returns a database error on connection failure.
    async fn is_version_referenced(&self, subject: &str, version: i32) -> Result<bool, KoraError>;
}

// -- Factory --

/// Connect to the backing `PostgreSQL` database and return a shared handle.
///
/// The returned handle has an open connection pool but migrations are not yet
/// applied — call [`Storage::migrate`] next.
///
/// # Errors
///
/// Returns [`StorageInitError`] if the database is unreachable.
pub async fn connect(cfg: &KoraConfig) -> Result<DynStorage, StorageInitError> {
    let pool = pg_connect(&cfg.database_url, cfg.db_pool_max)
        .await
        .map_err(|e| StorageInitError::Backend(e.to_string()))?;
    Ok(Arc::new(PgStorage::new(pool)))
}

// -- Postgres pool helpers --

/// Open a `PostgreSQL` connection pool (no migrations).
///
/// # Errors
///
/// Returns an error if the database is unreachable.
pub async fn pg_connect(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
}

/// Open a `PostgreSQL` pool and run embedded migrations.
///
/// Retained for `PostgreSQL`-specific integration tests that query the pool
/// directly; the application path uses [`connect`] + [`Storage::migrate`].
///
/// # Errors
///
/// Returns an error if the database is unreachable or migrations fail.
pub async fn create_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    let pool = pg_connect(database_url, max_connections).await?;
    sqlx::migrate!("./migrations/postgres").run(&pool).await?;
    Ok(pool)
}
