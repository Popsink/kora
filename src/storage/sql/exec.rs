//! Backend-neutral SQL execution surface.
//!
//! Each backend implements [`SqlExecutor`] once over its driver (pool, binding,
//! pagination, and any driver workarounds). The shared helpers and the simple
//! `Storage` methods are written against this trait, so they are identical
//! across backends.
//!
//! `SqlExecutor` is **never** used as `dyn` — only `Arc<dyn Storage>` is. Each
//! backend's `Storage` impl calls its own concrete executor, so dispatch is
//! static/monomorphized; the associated `Row` type (which differs per driver)
//! needs no object safety.

use async_trait::async_trait;

use super::{Bind, Row};
use crate::error::KoraError;

/// Run SQL against a backend, with neutral [`Bind`] params and [`Row`] results.
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// The backend's row type.
    type Row: Row + Send;

    /// Run a query and return all rows.
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] on any driver/query failure.
    async fn fetch_all(&self, sql: &str, params: &[Bind]) -> Result<Vec<Self::Row>, KoraError>;

    /// Run a query and return at most one row.
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] on any driver/query failure.
    async fn fetch_optional(
        &self,
        sql: &str,
        params: &[Bind],
    ) -> Result<Option<Self::Row>, KoraError>;

    /// Run a statement and return the number of affected rows.
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] on any driver/query failure.
    async fn execute(&self, sql: &str, params: &[Bind]) -> Result<u64, KoraError>;

    /// Run an ordered query windowed to `[offset, offset+limit)`; `limit < 0`
    /// means unbounded. `base_sql` must be a complete, ordered `SELECT` **without**
    /// a trailing `OFFSET`/`LIMIT`/`FETCH` clause.
    ///
    /// This is the seam that hides each backend's pagination idiom: Postgres
    /// appends `OFFSET/LIMIT`; Oracle pages with windowed `OFFSET … FETCH NEXT`
    /// queries (its driver caps a single fetch and cannot continue a cursor).
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] on any driver/query failure.
    async fn fetch_all_paged(
        &self,
        base_sql: &str,
        params: &[Bind],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Self::Row>, KoraError>;
}
