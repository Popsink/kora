//! `PostgreSQL` storage backend.
//!
//! [`PgStorage`] holds a `sqlx` [`PgPool`] and implements [`Storage`] by
//! delegating each non-lifecycle method to a per-domain module that owns the
//! `PostgreSQL` SQL and talks to `sqlx` directly. The handful of
//! multi-statement transactional methods live in those domain modules too, as
//! free functions that own the proven transaction logic.

pub mod compatibility;
pub mod mode;
pub mod references;
pub mod schemas;
pub mod subjects;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::error::KoraError;
use crate::storage::types::{
    CompatCheck, HardDeleteResult, NewSchema, SchemaVersion, SubjectVersion,
};
use crate::storage::{PoolStats, Storage};
use crate::types::SchemaReference;

/// `PostgreSQL`-backed [`Storage`] implementation.
#[derive(Clone)]
pub struct PgStorage {
    pool: PgPool,
}

impl PgStorage {
    /// Wrap an existing connection pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool (used by `PostgreSQL`-specific tests).
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// -- Shared query helpers --

/// A row selecting `sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text`
/// (in that exact order), decoded positionally.
pub(super) type SvRow = (i64, String, i32, String, String);

/// Map an [`SvRow`] to a [`SchemaVersion`] (references left empty — the caller
/// loads them separately).
pub(super) fn sv_from_row((id, subject, version, schema_type, schema): SvRow) -> SchemaVersion {
    SchemaVersion {
        id,
        subject,
        version,
        schema_type,
        schema,
        references: Vec::new(),
    }
}

/// Window an ordered `SELECT` to `[offset, offset+limit)`; `limit < 0` means
/// unbounded. `base_sql` must be a complete, ordered `SELECT` **without** a
/// trailing `OFFSET`/`LIMIT` clause. The interpolated values are internal
/// `i64`s, so the result stays safe to run via `AssertSqlSafe`.
pub(super) fn paged(base_sql: &str, offset: i64, limit: i64) -> String {
    let off = offset.max(0);
    if limit < 0 {
        format!("{base_sql} OFFSET {off}")
    } else {
        format!("{base_sql} OFFSET {off} LIMIT {limit}")
    }
}

/// Escape LIKE metacharacters and append `%`, mirroring the sibling modules.
pub(super) fn like_pattern(prefix: Option<&str>) -> Option<String> {
    prefix.filter(|p| !p.is_empty()).map(|p| {
        let escaped = p
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("{escaped}%")
    })
}

#[async_trait]
impl Storage for PgStorage {
    // -- Lifecycle --

    async fn migrate(&self) -> Result<(), KoraError> {
        sqlx::migrate!("./migrations/postgres")
            .run(&self.pool)
            .await
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    async fn ping(&self) -> Result<(), KoraError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(())
    }

    async fn schema_count(&self) -> Result<i64, KoraError> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM schema_contents")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    fn pool_stats(&self) -> PoolStats {
        PoolStats {
            size: self.pool.size(),
            idle: u32::try_from(self.pool.num_idle()).unwrap_or(u32::MAX),
        }
    }

    // -- Subjects --

    async fn list_subjects(
        &self,
        include_deleted: bool,
        deleted_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError> {
        subjects::list_subjects(self, include_deleted, deleted_only, prefix, offset, limit).await
    }

    async fn soft_delete_subject(&self, name: &str) -> Result<Vec<i32>, KoraError> {
        subjects::soft_delete_subject(self, name).await
    }

    async fn hard_delete_subject(&self, name: &str) -> Result<HardDeleteResult, KoraError> {
        subjects::hard_delete_subject(self, name).await
    }

    async fn find_subject_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>, KoraError> {
        subjects::find_subject_id_by_name(self, name, include_deleted).await
    }

    async fn subject_exists(&self, name: &str, include_deleted: bool) -> Result<bool, KoraError> {
        subjects::subject_exists(self, name, include_deleted).await
    }

    async fn subject_is_soft_deleted(&self, name: &str) -> Result<bool, KoraError> {
        subjects::subject_is_soft_deleted(self, name).await
    }

    // -- Schemas --

    async fn register_schema_atomically(
        &self,
        subject_name: &str,
        schema: &NewSchema<'_>,
        refs: &[SchemaReference],
        normalize: bool,
        compat: Option<CompatCheck>,
    ) -> Result<(i64, i32, bool), KoraError> {
        schemas::register_schema_atomically(self, subject_name, schema, refs, normalize, compat)
            .await
    }

    async fn find_all_active_versions(
        &self,
        subject: &str,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        schemas::find_all_active_versions(self, subject).await
    }

    async fn find_schema_by_subject_version(
        &self,
        subject: &str,
        version: i32,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        schemas::find_schema_by_subject_version(self, subject, version, include_deleted).await
    }

    async fn find_latest_schema_by_subject(
        &self,
        subject: &str,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        schemas::find_latest_schema_by_subject(self, subject, include_deleted).await
    }

    async fn find_schema_by_subject_id_and_fingerprint(
        &self,
        subject_id: i64,
        fingerprint: &str,
        normalize: bool,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        schemas::find_schema_by_subject_id_and_fingerprint(
            self,
            subject_id,
            fingerprint,
            normalize,
            include_deleted,
        )
        .await
    }

    async fn find_schema_by_id(&self, id: i64) -> Result<Option<(String, String)>, KoraError> {
        schemas::find_schema_by_id(self, id).await
    }

    async fn find_max_schema_id(&self) -> Result<i64, KoraError> {
        schemas::find_max_schema_id(self).await
    }

    async fn schema_exists(&self, id: i64) -> Result<bool, KoraError> {
        schemas::schema_exists(self, id).await
    }

    async fn find_subjects_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError> {
        schemas::find_subjects_by_schema_id(
            self,
            id,
            include_deleted,
            subject_filter,
            offset,
            limit,
        )
        .await
    }

    async fn find_versions_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SubjectVersion>, KoraError> {
        schemas::find_versions_by_schema_id(
            self,
            id,
            include_deleted,
            subject_filter,
            offset,
            limit,
        )
        .await
    }

    async fn list_schema_versions(
        &self,
        subject: &str,
        include_deleted: bool,
        deleted_only: bool,
        deleted_as_negative: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i32>, KoraError> {
        schemas::list_schema_versions(
            self,
            subject,
            include_deleted,
            deleted_only,
            deleted_as_negative,
            offset,
            limit,
        )
        .await
    }

    async fn list_schemas(
        &self,
        include_deleted: bool,
        latest_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        schemas::list_schemas(self, include_deleted, latest_only, prefix, offset, limit).await
    }

    async fn soft_delete_latest_schema(&self, subject: &str) -> Result<Option<i32>, KoraError> {
        schemas::soft_delete_latest_schema(self, subject).await
    }

    async fn soft_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        schemas::soft_delete_schema_version(self, subject, version).await
    }

    async fn hard_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        schemas::hard_delete_schema_version(self, subject, version).await
    }

    async fn version_is_soft_deleted(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<bool, KoraError> {
        schemas::version_is_soft_deleted(self, subject, version).await
    }

    async fn version_is_active(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        schemas::version_is_active(self, subject, version).await
    }

    // -- Compatibility config --

    async fn get_subject_level(&self, subject: &str) -> Result<Option<String>, KoraError> {
        compatibility::get_subject_level(self, subject).await
    }

    async fn get_global_level(&self) -> Result<String, KoraError> {
        compatibility::get_global_level(self).await
    }

    async fn set_global_level(&self, level: &str, normalize: bool) -> Result<String, KoraError> {
        compatibility::set_global_level(self, level, normalize).await
    }

    async fn reconcile_global_level(&self, level: &str) -> Result<String, KoraError> {
        compatibility::reconcile_global_level(self, level).await
    }

    async fn set_subject_level(
        &self,
        subject: &str,
        level: &str,
        normalize: bool,
    ) -> Result<String, KoraError> {
        compatibility::set_subject_level(self, subject, level, normalize).await
    }

    async fn delete_subject_level(
        &self,
        subject: &str,
    ) -> Result<Option<(String, bool)>, KoraError> {
        compatibility::delete_subject_level(self, subject).await
    }

    async fn get_global_normalize(&self) -> Result<bool, KoraError> {
        compatibility::get_global_normalize(self).await
    }

    async fn get_subject_normalize(&self, subject: &str) -> Result<Option<bool>, KoraError> {
        compatibility::get_subject_normalize(self, subject).await
    }

    async fn delete_global_level(&self) -> Result<(String, bool), KoraError> {
        compatibility::delete_global_level(self).await
    }

    // -- Mode --

    async fn get_global_mode(&self) -> Result<String, KoraError> {
        mode::get_global_mode(self).await
    }

    async fn set_global_mode(&self, mode_value: &str) -> Result<String, KoraError> {
        mode::set_global_mode(self, mode_value).await
    }

    async fn delete_global_mode(&self) -> Result<String, KoraError> {
        mode::delete_global_mode(self).await
    }

    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        mode::get_subject_mode(self, subject).await
    }

    async fn set_subject_mode(&self, subject: &str, mode_value: &str) -> Result<String, KoraError> {
        mode::set_subject_mode(self, subject, mode_value).await
    }

    async fn delete_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        mode::delete_subject_mode(self, subject).await
    }

    async fn delete_subject_mode_recursive(
        &self,
        subject: &str,
    ) -> Result<Option<String>, KoraError> {
        mode::delete_subject_mode_recursive(self, subject).await
    }

    async fn get_effective_mode(&self, subject: &str) -> Result<String, KoraError> {
        mode::get_effective_mode(self, subject).await
    }

    // -- References --

    async fn validate_references(&self, refs: &[SchemaReference]) -> Result<(), KoraError> {
        references::validate_references(self, refs).await
    }

    async fn find_references_by_schema_id(
        &self,
        content_id: i64,
    ) -> Result<Vec<SchemaReference>, KoraError> {
        references::find_references_by_schema_id(self, content_id).await
    }

    async fn find_references_for_schema_ids(
        &self,
        content_ids: &[i64],
    ) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
        references::find_references_for_schema_ids(self, content_ids).await
    }

    async fn find_referencing_schema_ids(
        &self,
        target_subject: &str,
        target_version: i32,
        include_deleted: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, KoraError> {
        references::find_referencing_schema_ids(
            self,
            target_subject,
            target_version,
            include_deleted,
            offset,
            limit,
        )
        .await
    }

    async fn is_version_referenced(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        references::is_version_referenced(self, subject, version).await
    }
}
