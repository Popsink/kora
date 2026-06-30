//! `PostgreSQL` storage backend — the default, fully-supported implementation.
//!
//! [`PgStorage`] holds a `sqlx` [`PgPool`] and implements [`Storage`] through the
//! shared SQL toolkit (`crate::storage::sql`): it provides a [`SqlExecutor`] over
//! its pool, then delegates each non-lifecycle `Storage` method to a per-domain
//! module that owns the dialect `PostgreSQL` SQL. The handful of multi-statement
//! transactional methods live in those domain modules too, as free functions that
//! own the proven transaction logic.

pub mod compatibility;
pub mod mode;
pub mod references;
pub mod schemas;
pub mod subjects;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::error::KoraError;
use crate::storage::sql::{Bind, Row, SqlExecutor};
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

// -- SQL toolkit: row wrapper, executor, shared row mapper --

/// Wraps a `sqlx` [`PgRow`](sqlx::postgres::PgRow) so it can be decoded
/// positionally through the backend-neutral [`Row`] trait.
pub struct PgRowWrap(sqlx::postgres::PgRow);

impl Row for PgRowWrap {
    fn get_i64(&self, idx: usize) -> Result<i64, KoraError> {
        sqlx::Row::try_get::<i64, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    fn get_i32(&self, idx: usize) -> Result<i32, KoraError> {
        sqlx::Row::try_get::<i32, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    fn get_str(&self, idx: usize) -> Result<String, KoraError> {
        sqlx::Row::try_get::<String, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    fn get_bool(&self, idx: usize) -> Result<bool, KoraError> {
        sqlx::Row::try_get::<bool, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    fn get_opt_i64(&self, idx: usize) -> Result<Option<i64>, KoraError> {
        sqlx::Row::try_get::<Option<i64>, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }

    fn get_opt_str(&self, idx: usize) -> Result<Option<String>, KoraError> {
        sqlx::Row::try_get::<Option<String>, _>(&self.0, idx)
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }
}

#[async_trait]
impl SqlExecutor for PgStorage {
    type Row = PgRowWrap;

    async fn fetch_all(&self, sql: &str, params: &[Bind]) -> Result<Vec<PgRowWrap>, KoraError> {
        // SAFETY (sqlx 0.9 `SqlSafeStr`): every caller builds `sql` from hardcoded
        // literals plus interpolated internal/typed values (filter literals, inlined
        // i64 id lists); all input-derived values are passed as binds, never spliced.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for b in params {
            q = match b {
                Bind::Str(s) => q.bind(s.as_str()),
                Bind::I64(i) => q.bind(*i),
                Bind::Bool(v) => q.bind(*v),
            };
        }
        Ok(q.fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(PgRowWrap)
            .collect())
    }

    async fn fetch_optional(
        &self,
        sql: &str,
        params: &[Bind],
    ) -> Result<Option<PgRowWrap>, KoraError> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for b in params {
            q = match b {
                Bind::Str(s) => q.bind(s.as_str()),
                Bind::I64(i) => q.bind(*i),
                Bind::Bool(v) => q.bind(*v),
            };
        }
        Ok(q.fetch_optional(&self.pool).await?.map(PgRowWrap))
    }

    async fn execute(&self, sql: &str, params: &[Bind]) -> Result<u64, KoraError> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for b in params {
            q = match b {
                Bind::Str(s) => q.bind(s.as_str()),
                Bind::I64(i) => q.bind(*i),
                Bind::Bool(v) => q.bind(*v),
            };
        }
        Ok(q.execute(&self.pool).await?.rows_affected())
    }

    async fn fetch_all_paged(
        &self,
        base_sql: &str,
        params: &[Bind],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<PgRowWrap>, KoraError> {
        let off = offset.max(0);
        let sql = if limit < 0 {
            format!("{base_sql} OFFSET {off}")
        } else {
            format!("{base_sql} OFFSET {off} LIMIT {limit}")
        };
        self.fetch_all(&sql, params).await
    }
}

/// Map a row selecting `sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text`
/// (in that exact order) to a [`SchemaVersion`].
pub(super) fn row_to_sv(r: &PgRowWrap) -> Result<SchemaVersion, KoraError> {
    Ok(SchemaVersion {
        id: r.get_i64(0)?,
        subject: r.get_str(1)?,
        version: r.get_i32(2)?,
        schema_type: r.get_str(3)?,
        schema: r.get_str(4)?,
        references: Vec::new(),
    })
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
