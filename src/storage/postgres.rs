//! `PostgreSQL` storage backend — the default, fully-supported implementation.
//!
//! [`PgStorage`] is a thin adapter: it holds a `sqlx` [`PgPool`] and implements
//! [`Storage`] by delegating to the dialect-specific query functions in the
//! sibling modules (`subjects`, `schemas`, `compatibility`, `mode`,
//! `references`). Those functions hold the proven `PostgreSQL` SQL and are left
//! untouched by the backend abstraction.

use async_trait::async_trait;
use sqlx::PgPool;

use super::schemas::{CompatCheck, NewSchema, SchemaVersion, SubjectVersion};
use super::subjects::HardDeleteResult;
use super::{PoolStats, Storage};
use super::{compatibility, mode, references, schemas, subjects};
use crate::error::KoraError;
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

#[async_trait]
impl Storage for PgStorage {
    // -- Lifecycle --

    async fn migrate(&self) -> Result<(), KoraError> {
        sqlx::migrate!("./migrations")
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
        Ok(subjects::list_subjects(
            &self.pool,
            include_deleted,
            deleted_only,
            prefix,
            offset,
            limit,
        )
        .await?)
    }

    async fn soft_delete_subject(&self, name: &str) -> Result<Vec<i32>, KoraError> {
        Ok(subjects::soft_delete_subject(&self.pool, name).await?)
    }

    async fn hard_delete_subject(&self, name: &str) -> Result<HardDeleteResult, KoraError> {
        Ok(subjects::hard_delete_subject(&self.pool, name).await?)
    }

    async fn find_subject_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>, KoraError> {
        Ok(subjects::find_subject_id_by_name(&self.pool, name, include_deleted).await?)
    }

    async fn subject_exists(&self, name: &str, include_deleted: bool) -> Result<bool, KoraError> {
        Ok(subjects::subject_exists(&self.pool, name, include_deleted).await?)
    }

    async fn subject_is_soft_deleted(&self, name: &str) -> Result<bool, KoraError> {
        Ok(subjects::subject_is_soft_deleted(&self.pool, name).await?)
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
        schemas::register_schema_atomically(
            &self.pool,
            subject_name,
            schema,
            refs,
            normalize,
            compat,
        )
        .await
    }

    async fn find_all_active_versions(
        &self,
        subject: &str,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        Ok(schemas::find_all_active_versions(&self.pool, subject).await?)
    }

    async fn find_schema_by_subject_version(
        &self,
        subject: &str,
        version: i32,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        Ok(
            schemas::find_schema_by_subject_version(&self.pool, subject, version, include_deleted)
                .await?,
        )
    }

    async fn find_latest_schema_by_subject(
        &self,
        subject: &str,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        Ok(schemas::find_latest_schema_by_subject(&self.pool, subject, include_deleted).await?)
    }

    async fn find_schema_by_subject_id_and_fingerprint(
        &self,
        subject_id: i64,
        fingerprint: &str,
        normalize: bool,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        Ok(schemas::find_schema_by_subject_id_and_fingerprint(
            &self.pool,
            subject_id,
            fingerprint,
            normalize,
            include_deleted,
        )
        .await?)
    }

    async fn find_schema_by_id(&self, id: i64) -> Result<Option<(String, String)>, KoraError> {
        Ok(schemas::find_schema_by_id(&self.pool, id).await?)
    }

    async fn find_max_schema_id(&self) -> Result<i64, KoraError> {
        Ok(schemas::find_max_schema_id(&self.pool).await?)
    }

    async fn schema_exists(&self, id: i64) -> Result<bool, KoraError> {
        Ok(schemas::schema_exists(&self.pool, id).await?)
    }

    async fn find_subjects_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError> {
        Ok(schemas::find_subjects_by_schema_id(
            &self.pool,
            id,
            include_deleted,
            subject_filter,
            offset,
            limit,
        )
        .await?)
    }

    async fn find_versions_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SubjectVersion>, KoraError> {
        Ok(schemas::find_versions_by_schema_id(
            &self.pool,
            id,
            include_deleted,
            subject_filter,
            offset,
            limit,
        )
        .await?)
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
        Ok(schemas::list_schema_versions(
            &self.pool,
            subject,
            include_deleted,
            deleted_only,
            deleted_as_negative,
            offset,
            limit,
        )
        .await?)
    }

    async fn list_schemas(
        &self,
        include_deleted: bool,
        latest_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        Ok(schemas::list_schemas(
            &self.pool,
            include_deleted,
            latest_only,
            prefix,
            offset,
            limit,
        )
        .await?)
    }

    async fn soft_delete_latest_schema(&self, subject: &str) -> Result<Option<i32>, KoraError> {
        Ok(schemas::soft_delete_latest_schema(&self.pool, subject).await?)
    }

    async fn soft_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        Ok(schemas::soft_delete_schema_version(&self.pool, subject, version).await?)
    }

    async fn hard_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        Ok(schemas::hard_delete_schema_version(&self.pool, subject, version).await?)
    }

    async fn version_is_soft_deleted(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<bool, KoraError> {
        Ok(schemas::version_is_soft_deleted(&self.pool, subject, version).await?)
    }

    async fn version_is_active(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        Ok(schemas::version_is_active(&self.pool, subject, version).await?)
    }

    // -- Compatibility config --

    async fn get_subject_level(&self, subject: &str) -> Result<Option<String>, KoraError> {
        Ok(compatibility::get_subject_level(&self.pool, subject).await?)
    }

    async fn get_global_level(&self) -> Result<String, KoraError> {
        Ok(compatibility::get_global_level(&self.pool).await?)
    }

    async fn set_global_level(&self, level: &str, normalize: bool) -> Result<String, KoraError> {
        Ok(compatibility::set_global_level(&self.pool, level, normalize).await?)
    }

    async fn reconcile_global_level(&self, level: &str) -> Result<String, KoraError> {
        Ok(compatibility::reconcile_global_level(&self.pool, level).await?)
    }

    async fn set_subject_level(
        &self,
        subject: &str,
        level: &str,
        normalize: bool,
    ) -> Result<String, KoraError> {
        Ok(compatibility::set_subject_level(&self.pool, subject, level, normalize).await?)
    }

    async fn delete_subject_level(
        &self,
        subject: &str,
    ) -> Result<Option<(String, bool)>, KoraError> {
        Ok(compatibility::delete_subject_level(&self.pool, subject).await?)
    }

    async fn get_global_normalize(&self) -> Result<bool, KoraError> {
        Ok(compatibility::get_global_normalize(&self.pool).await?)
    }

    async fn get_subject_normalize(&self, subject: &str) -> Result<Option<bool>, KoraError> {
        Ok(compatibility::get_subject_normalize(&self.pool, subject).await?)
    }

    async fn delete_global_level(&self) -> Result<(String, bool), KoraError> {
        Ok(compatibility::delete_global_level(&self.pool).await?)
    }

    // -- Mode --

    async fn get_global_mode(&self) -> Result<String, KoraError> {
        Ok(mode::get_global_mode(&self.pool).await?)
    }

    async fn set_global_mode(&self, mode_value: &str) -> Result<String, KoraError> {
        Ok(mode::set_global_mode(&self.pool, mode_value).await?)
    }

    async fn delete_global_mode(&self) -> Result<String, KoraError> {
        Ok(mode::delete_global_mode(&self.pool).await?)
    }

    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        Ok(mode::get_subject_mode(&self.pool, subject).await?)
    }

    async fn set_subject_mode(&self, subject: &str, mode_value: &str) -> Result<String, KoraError> {
        Ok(mode::set_subject_mode(&self.pool, subject, mode_value).await?)
    }

    async fn delete_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        Ok(mode::delete_subject_mode(&self.pool, subject).await?)
    }

    async fn delete_subject_mode_recursive(
        &self,
        subject: &str,
    ) -> Result<Option<String>, KoraError> {
        Ok(mode::delete_subject_mode_recursive(&self.pool, subject).await?)
    }

    async fn get_effective_mode(&self, subject: &str) -> Result<String, KoraError> {
        Ok(mode::get_effective_mode(&self.pool, subject).await?)
    }

    // -- References --

    async fn validate_references(&self, refs: &[SchemaReference]) -> Result<(), KoraError> {
        references::validate_references(&self.pool, refs).await
    }

    async fn find_references_by_schema_id(
        &self,
        content_id: i64,
    ) -> Result<Vec<SchemaReference>, KoraError> {
        Ok(references::find_references_by_schema_id(&self.pool, content_id).await?)
    }

    async fn find_references_for_schema_ids(
        &self,
        content_ids: &[i64],
    ) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
        Ok(references::find_references_for_schema_ids(&self.pool, content_ids).await?)
    }

    async fn find_referencing_schema_ids(
        &self,
        target_subject: &str,
        target_version: i32,
        include_deleted: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, KoraError> {
        Ok(references::find_referencing_schema_ids(
            &self.pool,
            target_subject,
            target_version,
            include_deleted,
            offset,
            limit,
        )
        .await?)
    }

    async fn is_version_referenced(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        Ok(references::is_version_referenced(&self.pool, subject, version).await?)
    }
}
