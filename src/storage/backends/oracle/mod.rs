//! Oracle storage backend (behind the `oracle` cargo feature).
//!
//! Uses the **pure-Rust** [`oracle_rs`] driver (Oracle TNS/TTC protocol, no OCI /
//! ODPI-C / Instant Client) with a [`deadpool_oracle`] connection pool. The whole
//! backend is async-native — no `spawn_blocking` bridge — and an Oracle-enabled
//! build remains a single self-contained binary that connects over TCP, exactly
//! like the Postgres path.
//!
//! ## Layout
//!
//! This module is a thin adapter: [`OracleStorage`] (pool + [`conn`] borrow), the
//! [`SqlExecutor`] impl that the shared toolkit runs through, the lifecycle
//! methods, and an [`Storage`] impl whose non-lifecycle methods each delegate in
//! one line to a per-domain SQL module (`subjects`, `schemas`, `compatibility`,
//! `mode`, `references`). The `oracle_rs` driver workarounds and the row-decode
//! machinery live in [`driver`].
//!
//! [`conn`]: OracleStorage::conn
//!
//! ## Dialect translation
//!
//! The query layer is a hand-written Oracle translation of the `PostgreSQL`
//! statements in the sibling modules. Conventions used throughout:
//!
//! * **String values are bound** as `:1`, `:2`, … (each a distinct placeholder —
//!   a value needed twice gets two placeholders so binds never collide).
//!   **Integers and booleans are inlined** as literals (internal/typed
//!   `i32`/`i64`/`bool`, no injection risk). Booleans map to `NUMBER(1)` `0`/`1`.
//! * No `RETURNING ... INTO`: inserted identifiers are read back with a follow-up
//!   `SELECT`; upserts echo the value just written.
//! * `now()` → `SYSTIMESTAMP`; `ON CONFLICT` → `UPDATE`-then-`INSERT` with a
//!   unique-violation retry; `DISTINCT ON` → `ROW_NUMBER()`; `^@` → `INSTR(..)=1`;
//!   `SELECT EXISTS(..)` → `SELECT CASE WHEN EXISTS(..) THEN 1 ELSE 0 END FROM dual`.
//! * The registry-mode column is named `registry_mode` here (it is `mode` on
//!   Postgres) because `MODE` is an Oracle reserved word. The two backends own
//!   independent schemas and queries, so this never affects Postgres; the trait
//!   surface (`get_global_mode`, the `mode: &str` arguments) is unchanged.
//!
//! Transactions: the driver runs with autocommit off; each write commits
//! explicitly, and the deadpool manager rolls back any pending work when a
//! connection is returned to the pool, so a failed write cannot leak.

pub mod compatibility;
pub mod driver;
pub mod mode;
pub mod references;
pub mod schemas;
pub mod subjects;

use async_trait::async_trait;
use deadpool_oracle::{Object, Pool, PoolBuilder};
use oracle_rs::Config;

use crate::error::KoraError;
use crate::storage::sql::{Bind, SqlExecutor};
use crate::storage::types::{
    CompatCheck, HardDeleteResult, NewSchema, SchemaVersion, SubjectVersion,
};
use crate::storage::{PoolStats, Storage};
use crate::types::SchemaReference;

use driver::{OraRow, lower, query_all, val_i64};

/// Oracle-backed [`Storage`] implementation.
#[derive(Clone)]
pub struct OracleStorage {
    pool: Pool,
}

impl OracleStorage {
    /// Build a connection pool to the Oracle instance from its components.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool cannot be created (e.g. invalid parameters).
    /// Connections are established lazily, so an unreachable database surfaces on
    /// first use rather than here.
    // `async` for symmetry with the Postgres path and the `connect` factory; the
    // pool builds synchronously (connections are lazy).
    #[allow(clippy::unused_async)]
    pub async fn connect(
        host: &str,
        port: u16,
        service: &str,
        username: &str,
        password: &str,
        max_connections: u32,
    ) -> Result<Self, KoraError> {
        let config = Config::new(host, port, service, username, password);
        let pool = PoolBuilder::new(config)
            .max_size(max_connections.max(1) as usize)
            .build()
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Borrow a pooled connection.
    pub(super) async fn conn(&self) -> Result<Object, KoraError> {
        self.pool
            .get()
            .await
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }
}

// -- SQL toolkit: executor --

#[async_trait]
impl SqlExecutor for OracleStorage {
    type Row = OraRow;

    async fn fetch_all(&self, sql: &str, params: &[Bind]) -> Result<Vec<OraRow>, KoraError> {
        let conn = self.conn().await?;
        let result = conn.query(sql, &lower(params)).await?;
        Ok(result.into_iter().map(OraRow).collect())
    }

    async fn fetch_optional(
        &self,
        sql: &str,
        params: &[Bind],
    ) -> Result<Option<OraRow>, KoraError> {
        let conn = self.conn().await?;
        let result = conn.query(sql, &lower(params)).await?;
        Ok(result.into_iter().next().map(OraRow))
    }

    async fn execute(&self, sql: &str, params: &[Bind]) -> Result<u64, KoraError> {
        // The driver runs with autocommit off and the deadpool manager rolls back
        // pending work on connection return, so commit the single statement here —
        // mirroring sqlx's per-statement autocommit on the Postgres path. The
        // shared toolkit only routes self-contained single-statement writes through
        // `execute`; multi-statement transactions stay hand-written below.
        let conn = self.conn().await?;
        let affected = conn.execute_dml_sql(sql, &lower(params)).await?;
        conn.commit().await?;
        Ok(affected)
    }

    async fn fetch_all_paged(
        &self,
        base_sql: &str,
        params: &[Bind],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<OraRow>, KoraError> {
        let conn = self.conn().await?;
        let result = query_all(&conn, base_sql, &lower(params), offset, limit).await?;
        Ok(result.into_iter().map(OraRow).collect())
    }
}

#[async_trait]
impl Storage for OracleStorage {
    // -- Lifecycle --

    async fn migrate(&self) -> Result<(), KoraError> {
        let conn = self.conn().await?;
        conn.execute_plsql(driver::MIGRATION_001, &[]).await?;
        Ok(())
    }

    async fn ping(&self) -> Result<(), KoraError> {
        let conn = self.conn().await?;
        conn.query("SELECT 1 FROM dual", &[]).await?;
        Ok(())
    }

    async fn schema_count(&self) -> Result<i64, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query("SELECT COUNT(*) FROM schema_contents", &[])
            .await?;
        result
            .first()
            .and_then(|row| val_i64(row.get(0)))
            .ok_or_else(|| KoraError::BackendDataStore("count returned no row".to_owned()))
    }

    fn pool_stats(&self) -> PoolStats {
        let st = self.pool.status();
        PoolStats {
            size: u32::try_from(st.size).unwrap_or(u32::MAX),
            idle: u32::try_from(st.available).unwrap_or(u32::MAX),
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

    async fn set_global_mode(&self, mode: &str) -> Result<String, KoraError> {
        mode::set_global_mode(self, mode).await
    }

    async fn delete_global_mode(&self) -> Result<String, KoraError> {
        mode::delete_global_mode(self).await
    }

    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        mode::get_subject_mode(self, subject).await
    }

    async fn set_subject_mode(&self, subject: &str, mode: &str) -> Result<String, KoraError> {
        mode::set_subject_mode(self, subject, mode).await
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
