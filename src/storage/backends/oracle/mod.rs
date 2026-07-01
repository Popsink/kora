//! Oracle storage backend (behind the `oracle` cargo feature).
//!
//! Uses the mature [`oracle`] crate (kubo/rust-oracle, ODPI-C / OCI), which
//! requires the **Oracle Instant Client** shared libraries at runtime — an
//! Oracle-enabled build is therefore **not** a self-contained static binary (see
//! `dockerfiles/oracle.Dockerfile`). The driver is **blocking**, so every database operation
//! is bridged onto the async runtime with a single [`tokio::task::spawn_blocking`]
//! per [`Storage`] call (see [`with_conn`](OracleStorage::with_conn)); a
//! [`Semaphore`] sized to the pool bounds the number of concurrent blocking DB
//! operations so the shared tokio blocking-thread pool is never starved.
//!
//! ## Layout
//!
//! This module is a thin adapter: [`OracleStorage`] (the native
//! [`oracle::pool::Pool`] + the concurrency semaphore), the lifecycle methods,
//! and a [`Storage`] impl whose non-lifecycle methods each materialize their
//! arguments as owned values and delegate, inside a `with_conn` closure, to a
//! synchronous per-domain SQL function (`subjects`, `schemas`, `compatibility`,
//! `mode`, `references`). The bind/decode helpers live in [`driver`].
//!
//! Oracle deliberately does **not** go through the shared async `SqlExecutor`
//! seam (which stays Postgres-only): that seam takes one pooled connection per
//! query, which would break the multi-statement transactions here (each would run
//! on a different connection). Instead the per-domain functions take a borrowed
//! `&oracle::Connection` and run every statement of a logical operation on it.
//!
//! ## Dialect translation
//!
//! See [`driver`] for the SQL dialect conventions (binding, CLOB binds,
//! `SYSTIMESTAMP`, `registry_mode`, existence queries, paging).
//!
//! Transactions: the driver runs with autocommit off. Each transactional
//! operation commits explicitly on success and **rolls back on every error path**
//! before returning — the native OCI pool does not roll back a dirty connection
//! on return (unlike a recycling pool), so a leaked pending transaction would be
//! inherited by the next borrower.

pub mod compatibility;
pub mod driver;
pub mod mode;
pub mod references;
pub mod schemas;
pub mod subjects;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::error::KoraError;
use crate::storage::types::{
    CompatCheck, HardDeleteResult, NewSchema, SchemaVersion, SubjectVersion,
};
use crate::storage::{PoolStats, Storage};
use crate::types::SchemaReference;

use driver::map_ora;

/// Owned mirror of [`NewSchema`] (which borrows `&str`), so a registration can be
/// moved into the `Send + 'static` blocking closure.
struct NewSchemaOwned {
    schema_type: String,
    schema_text: String,
    canonical_form: String,
    fingerprint: String,
    raw_fingerprint: String,
}

impl NewSchemaOwned {
    fn new(schema: &NewSchema<'_>) -> Self {
        Self {
            schema_type: schema.schema_type.to_owned(),
            schema_text: schema.schema_text.to_owned(),
            canonical_form: schema.canonical_form.to_owned(),
            fingerprint: schema.fingerprint.to_owned(),
            raw_fingerprint: schema.raw_fingerprint.to_owned(),
        }
    }

    /// Borrow the owned fields back as a [`NewSchema`] for the per-domain fns.
    fn as_ref(&self) -> NewSchema<'_> {
        NewSchema {
            schema_type: &self.schema_type,
            schema_text: &self.schema_text,
            canonical_form: &self.canonical_form,
            fingerprint: &self.fingerprint,
            raw_fingerprint: &self.raw_fingerprint,
        }
    }
}

/// Oracle-backed [`Storage`] implementation.
///
/// Holds the native connection pool (cheap to clone — an `Arc` internally) and a
/// semaphore that caps concurrent blocking DB operations at the pool size.
#[derive(Clone)]
pub struct OracleStorage {
    pool: oracle::pool::Pool,
    sem: Arc<Semaphore>,
}

impl OracleStorage {
    /// Build a connection pool to the Oracle instance from its components.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool cannot be created (e.g. the Instant Client
    /// libraries are missing, or the parameters are invalid). The pool opens its
    /// minimum connections eagerly, so an unreachable database surfaces here.
    pub async fn connect(
        host: &str,
        port: u16,
        service: &str,
        username: &str,
        password: &str,
        max_connections: u32,
    ) -> Result<Self, KoraError> {
        // Serialises OCI session-pool creation across threads (see the lock use below).
        static POOL_CREATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let connect_string = format!("//{host}:{port}/{service}");
        let (username, password) = (username.to_owned(), password.to_owned());

        // Building the pool is a blocking OCI call; run it off the async runtime.
        let pool = tokio::task::spawn_blocking(move || {
            // Creating an OCI session pool (`dpiPool_create` / `OCISessionPoolCreate`)
            // concurrently from multiple threads in one process races and
            // intermittently fails with `ORA-24416: Invalid session Poolname`.
            // Production builds a single pool at startup so this never bites, but the
            // test suite builds many pools in parallel — serialise creation
            // process-wide. The lock guards only the brief build; connection checkout
            // (`Pool::get`) is unaffected.
            let _guard = POOL_CREATE_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let max = max_connections.max(1);
            let min = max.min(2);
            let inc = u32::from(max > 1);
            let build = || {
                oracle::pool::PoolBuilder::new(
                    username.clone(),
                    password.clone(),
                    connect_string.clone(),
                )
                .min_connections(min)
                .max_connections(max)
                .connection_increment(inc)
                .get_mode(oracle::pool::GetMode::TimedWait(Duration::from_secs(30)))
                .stmt_cache_size(20)
                .build()
            };
            // Oracle Free's listener transiently rejects connection handoffs under
            // bursty pool creation (ORA-12516 & friends); retry with a short backoff.
            let mut attempt = 0u32;
            loop {
                match build() {
                    Ok(pool) => break Ok(pool),
                    Err(e) if attempt < 9 && driver::is_transient_connect(&e) => {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(u64::from(attempt) * 250));
                    }
                    Err(e) => break Err(e),
                }
            }
        })
        .await
        .map_err(|e| KoraError::BackendDataStore(format!("oracle pool build task failed: {e}")))?
        .map_err(map_ora)?;

        let sem = Arc::new(Semaphore::new(max_connections.max(1) as usize));
        Ok(Self { pool, sem })
    }

    /// Run a blocking database operation on a pooled connection.
    ///
    /// Acquires a semaphore permit (bounding concurrent blocking ops to the pool
    /// size), then `spawn_blocking`s the closure: it checks out a connection
    /// **inside** the blocking task and hands it to `f`. The `Connection`,
    /// `ResultSet` and `Row` values never cross a `.await` — `f` drains everything
    /// it needs into owned Kora types before returning.
    async fn with_conn<T, F>(&self, f: F) -> Result<T, KoraError>
    where
        F: FnOnce(&oracle::Connection) -> Result<T, KoraError> + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))?;
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = pool.get().map_err(map_ora)?;
            f(&conn)
        })
        .await
        .map_err(|e| KoraError::BackendDataStore(format!("oracle blocking task failed: {e}")))?
    }
}

#[async_trait]
impl Storage for OracleStorage {
    // -- Lifecycle --

    async fn migrate(&self) -> Result<(), KoraError> {
        self.with_conn(|conn| {
            conn.execute(driver::MIGRATION_001, &[]).map_err(map_ora)?;
            Ok(())
        })
        .await
    }

    async fn ping(&self) -> Result<(), KoraError> {
        self.with_conn(|conn| {
            conn.query("SELECT 1 FROM dual", &[]).map_err(map_ora)?;
            Ok(())
        })
        .await
    }

    async fn schema_count(&self) -> Result<i64, KoraError> {
        self.with_conn(|conn| {
            let row = conn
                .query("SELECT COUNT(*) FROM schema_contents", &[])
                .map_err(map_ora)?
                .next()
                .ok_or_else(|| KoraError::BackendDataStore("count returned no row".to_owned()))?
                .map_err(map_ora)?;
            row.get::<usize, i64>(0).map_err(map_ora)
        })
        .await
    }

    fn pool_stats(&self) -> PoolStats {
        let size = self.pool.open_count().unwrap_or(0);
        let busy = self.pool.busy_count().unwrap_or(0);
        PoolStats {
            size,
            idle: size.saturating_sub(busy),
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
        let prefix = prefix.map(str::to_owned);
        self.with_conn(move |conn| {
            subjects::list_subjects(
                conn,
                include_deleted,
                deleted_only,
                prefix.as_deref(),
                offset,
                limit,
            )
        })
        .await
    }

    async fn soft_delete_subject(&self, name: &str) -> Result<Vec<i32>, KoraError> {
        let name = name.to_owned();
        self.with_conn(move |conn| subjects::soft_delete_subject(conn, &name))
            .await
    }

    async fn hard_delete_subject(&self, name: &str) -> Result<HardDeleteResult, KoraError> {
        let name = name.to_owned();
        self.with_conn(move |conn| subjects::hard_delete_subject(conn, &name))
            .await
    }

    async fn find_subject_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>, KoraError> {
        let name = name.to_owned();
        self.with_conn(move |conn| subjects::find_subject_id_by_name(conn, &name, include_deleted))
            .await
    }

    async fn subject_exists(&self, name: &str, include_deleted: bool) -> Result<bool, KoraError> {
        let name = name.to_owned();
        self.with_conn(move |conn| subjects::subject_exists(conn, &name, include_deleted))
            .await
    }

    async fn subject_is_soft_deleted(&self, name: &str) -> Result<bool, KoraError> {
        let name = name.to_owned();
        self.with_conn(move |conn| subjects::subject_is_soft_deleted(conn, &name))
            .await
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
        let subject_name = subject_name.to_owned();
        let schema = NewSchemaOwned::new(schema);
        let refs = refs.to_vec();
        self.with_conn(move |conn| {
            schemas::register_schema_atomically(
                conn,
                &subject_name,
                &schema.as_ref(),
                &refs,
                normalize,
                compat.as_ref(),
            )
        })
        .await
    }

    async fn find_all_active_versions(
        &self,
        subject: &str,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::find_all_active_versions(conn, &subject))
            .await
    }

    async fn find_schema_by_subject_version(
        &self,
        subject: &str,
        version: i32,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| {
            schemas::find_schema_by_subject_version(conn, &subject, version, include_deleted)
        })
        .await
    }

    async fn find_latest_schema_by_subject(
        &self,
        subject: &str,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| {
            schemas::find_latest_schema_by_subject(conn, &subject, include_deleted)
        })
        .await
    }

    async fn find_schema_by_subject_id_and_fingerprint(
        &self,
        subject_id: i64,
        fingerprint: &str,
        normalize: bool,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let fingerprint = fingerprint.to_owned();
        self.with_conn(move |conn| {
            schemas::find_schema_by_subject_id_and_fingerprint(
                conn,
                subject_id,
                &fingerprint,
                normalize,
                include_deleted,
            )
        })
        .await
    }

    async fn find_schema_by_id(&self, id: i64) -> Result<Option<(String, String)>, KoraError> {
        self.with_conn(move |conn| schemas::find_schema_by_id(conn, id))
            .await
    }

    async fn find_max_schema_id(&self) -> Result<i64, KoraError> {
        self.with_conn(schemas::find_max_schema_id).await
    }

    async fn schema_exists(&self, id: i64) -> Result<bool, KoraError> {
        self.with_conn(move |conn| schemas::schema_exists(conn, id))
            .await
    }

    async fn find_subjects_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError> {
        let subject_filter = subject_filter.map(str::to_owned);
        self.with_conn(move |conn| {
            schemas::find_subjects_by_schema_id(
                conn,
                id,
                include_deleted,
                subject_filter.as_deref(),
                offset,
                limit,
            )
        })
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
        let subject_filter = subject_filter.map(str::to_owned);
        self.with_conn(move |conn| {
            schemas::find_versions_by_schema_id(
                conn,
                id,
                include_deleted,
                subject_filter.as_deref(),
                offset,
                limit,
            )
        })
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
        let subject = subject.to_owned();
        self.with_conn(move |conn| {
            schemas::list_schema_versions(
                conn,
                &subject,
                include_deleted,
                deleted_only,
                deleted_as_negative,
                offset,
                limit,
            )
        })
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
        let prefix = prefix.map(str::to_owned);
        self.with_conn(move |conn| {
            schemas::list_schemas(
                conn,
                include_deleted,
                latest_only,
                prefix.as_deref(),
                offset,
                limit,
            )
        })
        .await
    }

    async fn soft_delete_latest_schema(&self, subject: &str) -> Result<Option<i32>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::soft_delete_latest_schema(conn, &subject))
            .await
    }

    async fn soft_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::soft_delete_schema_version(conn, &subject, version))
            .await
    }

    async fn hard_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::hard_delete_schema_version(conn, &subject, version))
            .await
    }

    async fn version_is_soft_deleted(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<bool, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::version_is_soft_deleted(conn, &subject, version))
            .await
    }

    async fn version_is_active(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| schemas::version_is_active(conn, &subject, version))
            .await
    }

    // -- Compatibility config --

    async fn get_subject_level(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| compatibility::get_subject_level(conn, &subject))
            .await
    }

    async fn get_global_level(&self) -> Result<String, KoraError> {
        self.with_conn(compatibility::get_global_level).await
    }

    async fn set_global_level(&self, level: &str, normalize: bool) -> Result<String, KoraError> {
        let level = level.to_owned();
        self.with_conn(move |conn| compatibility::set_global_level(conn, &level, normalize))
            .await
    }

    async fn reconcile_global_level(&self, level: &str) -> Result<String, KoraError> {
        let level = level.to_owned();
        self.with_conn(move |conn| compatibility::reconcile_global_level(conn, &level))
            .await
    }

    async fn set_subject_level(
        &self,
        subject: &str,
        level: &str,
        normalize: bool,
    ) -> Result<String, KoraError> {
        let subject = subject.to_owned();
        let level = level.to_owned();
        self.with_conn(move |conn| {
            compatibility::set_subject_level(conn, &subject, &level, normalize)
        })
        .await
    }

    async fn delete_subject_level(
        &self,
        subject: &str,
    ) -> Result<Option<(String, bool)>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| compatibility::delete_subject_level(conn, &subject))
            .await
    }

    async fn get_global_normalize(&self) -> Result<bool, KoraError> {
        self.with_conn(compatibility::get_global_normalize).await
    }

    async fn get_subject_normalize(&self, subject: &str) -> Result<Option<bool>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| compatibility::get_subject_normalize(conn, &subject))
            .await
    }

    async fn delete_global_level(&self) -> Result<(String, bool), KoraError> {
        self.with_conn(compatibility::delete_global_level).await
    }

    // -- Mode --

    async fn get_global_mode(&self) -> Result<String, KoraError> {
        self.with_conn(mode::get_global_mode).await
    }

    async fn set_global_mode(&self, mode: &str) -> Result<String, KoraError> {
        let mode = mode.to_owned();
        self.with_conn(move |conn| mode::set_global_mode(conn, &mode))
            .await
    }

    async fn delete_global_mode(&self) -> Result<String, KoraError> {
        self.with_conn(mode::delete_global_mode).await
    }

    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| mode::get_subject_mode(conn, &subject))
            .await
    }

    async fn set_subject_mode(&self, subject: &str, mode: &str) -> Result<String, KoraError> {
        let subject = subject.to_owned();
        let mode = mode.to_owned();
        self.with_conn(move |conn| mode::set_subject_mode(conn, &subject, &mode))
            .await
    }

    async fn delete_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| mode::delete_subject_mode(conn, &subject))
            .await
    }

    async fn delete_subject_mode_recursive(
        &self,
        subject: &str,
    ) -> Result<Option<String>, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| mode::delete_subject_mode_recursive(conn, &subject))
            .await
    }

    async fn get_effective_mode(&self, subject: &str) -> Result<String, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| mode::get_effective_mode(conn, &subject))
            .await
    }

    // -- References --

    async fn validate_references(&self, refs: &[SchemaReference]) -> Result<(), KoraError> {
        let refs = refs.to_vec();
        self.with_conn(move |conn| references::validate_references(conn, &refs))
            .await
    }

    async fn find_references_by_schema_id(
        &self,
        content_id: i64,
    ) -> Result<Vec<SchemaReference>, KoraError> {
        self.with_conn(move |conn| references::find_references_by_schema_id(conn, content_id))
            .await
    }

    async fn find_references_for_schema_ids(
        &self,
        content_ids: &[i64],
    ) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
        let content_ids = content_ids.to_vec();
        self.with_conn(move |conn| references::find_references_for_schema_ids(conn, &content_ids))
            .await
    }

    async fn find_referencing_schema_ids(
        &self,
        target_subject: &str,
        target_version: i32,
        include_deleted: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, KoraError> {
        let target_subject = target_subject.to_owned();
        self.with_conn(move |conn| {
            references::find_referencing_schema_ids(
                conn,
                &target_subject,
                target_version,
                include_deleted,
                offset,
                limit,
            )
        })
        .await
    }

    async fn is_version_referenced(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        let subject = subject.to_owned();
        self.with_conn(move |conn| references::is_version_referenced(conn, &subject, version))
            .await
    }
}
