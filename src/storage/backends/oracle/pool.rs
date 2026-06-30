//! Cursor-leak-safe connection pool for the `oracle_rs` driver.
//!
//! `oracle_rs` 0.1.7 never closes server-side cursors (see [`super::driver`]):
//! `mark_cursor_closed` only resets the client-side `cursor_id`, the
//! `CloseCursors` TTC op is dead code, and `Statement` has no `Drop` — so a
//! connection leaks one server cursor per executed statement, and Oracle drops
//! the session once it reaches `open_cursors` (~300). Tearing down the TNS
//! session (dropping the connection) is the **only** lever the driver exposes to
//! release those cursors.
//!
//! So this module wraps each pooled connection in [`CountedConn`], which counts
//! every cursor-opening call (`query` / `execute_dml_sql` / `execute_plsql`), and
//! a custom [`CountingManager`] retires (drops + recreates) the connection in
//! `recycle` — on the next checkout — once it crosses a configurable execution
//! threshold kept safely below `open_cursors`. This is the per-execution analogue
//! of `HikariCP`'s `maxLifetime`. The threshold must stay below
//! `open_cursors` minus the largest number of statements a single checkout can
//! run (e.g. a paginated listing); the default is sized for the common
//! `open_cursors = 300`.
//!
//! [`CountedConn`] also guards against `oracle_rs`'s cancellation hazard
//! (upstream issue #11): a query future dropped mid-round-trip leaves the TNS
//! stream desynced, and the bad connection would otherwise hang the next caller.
//! The [`CancelGuard`] marks such a connection poisoned so `recycle` discards it.

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

use deadpool::managed::{self, BuildError, Manager, Metrics, RecycleError, RecycleResult};
use oracle_rs::{BindParam, Config, Connection, Error, PlsqlResult, QueryResult, Value};

/// A pooled Oracle connection that counts executed statements and self-poisons on
/// mid-flight cancellation.
///
/// The inherent `query` / `execute_dml_sql` / `execute_plsql` methods **shadow**
/// the ones reached through [`Deref`], so every existing call site counts its
/// statement without any change. All other `Connection` methods (`commit`,
/// `rollback`, `read_clob`, `is_closed`, …) resolve through the [`Deref`] and are
/// left untouched — they open no server cursor.
pub struct CountedConn {
    conn: Connection,
    /// Statements executed on this physical connection (≈ leaked server cursors).
    execs: AtomicUsize,
    /// Set when a query future was dropped mid-round-trip, desyncing the stream.
    poisoned: AtomicBool,
}

impl CountedConn {
    fn new(conn: Connection) -> Self {
        Self {
            conn,
            execs: AtomicUsize::new(0),
            poisoned: AtomicBool::new(false),
        }
    }

    /// Statements executed so far on this connection.
    pub(super) fn execs(&self) -> usize {
        self.execs.load(Relaxed)
    }

    /// Run a query — counts the leaked cursor and guards against cancellation.
    pub(super) async fn query(&self, sql: &str, params: &[Value]) -> Result<QueryResult, Error> {
        self.execs.fetch_add(1, Relaxed);
        let guard = CancelGuard(&self.poisoned);
        let result = self.conn.query(sql, params).await;
        guard.disarm();
        result
    }

    /// Run a DML statement (INSERT/UPDATE/DELETE) — counted and cancel-guarded.
    pub(super) async fn execute_dml_sql(&self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        self.execs.fetch_add(1, Relaxed);
        let guard = CancelGuard(&self.poisoned);
        let result = self.conn.execute_dml_sql(sql, params).await;
        guard.disarm();
        result
    }

    /// Run a PL/SQL block — counted and cancel-guarded.
    pub(super) async fn execute_plsql(
        &self,
        sql: &str,
        params: &[BindParam],
    ) -> Result<PlsqlResult, Error> {
        self.execs.fetch_add(1, Relaxed);
        let guard = CancelGuard(&self.poisoned);
        let result = self.conn.execute_plsql(sql, params).await;
        guard.disarm();
        result
    }
}

impl Deref for CountedConn {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

/// Marks the connection poisoned if dropped before [`disarm`](Self::disarm) — i.e.
/// the awaited statement future was cancelled mid-round-trip, leaving the TNS
/// stream desynced (`oracle_rs` issue #11). A completed round-trip (whether `Ok`
/// or a clean protocol `Err`) disarms it.
struct CancelGuard<'a>(&'a AtomicBool);

impl CancelGuard<'_> {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
    }
}

/// True for transient connection-*establishment* failures worth retrying: a
/// constrained Oracle (e.g. Oracle Free sharing a 2-vCPU CI runner with the test
/// workload) momentarily refuses or drops new sessions under concurrent load.
/// Without this, a refused checkout surfaces directly as a request error.
fn is_transient_connect(e: &Error) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    m.contains("connection refused")
        || m.contains("connection reset")
        || m.contains("i/o error")
        || m.contains("timed out")
        || m.contains("broken pipe")
        || m.contains("listener")
}

/// deadpool manager that creates [`CountedConn`]s and retires them once they have
/// executed `max_execs` statements (≈ leaked cursors) or have been poisoned.
pub struct CountingManager {
    config: Config,
    max_execs: usize,
}

impl Manager for CountingManager {
    type Type = CountedConn;
    type Error = Error;

    async fn create(&self) -> Result<CountedConn, Error> {
        // Retry transient connection-establishment failures with a short backoff:
        // a constrained Oracle momentarily refusing new sessions under concurrent
        // load (common on a small CI runner) would otherwise fail the request
        // outright — register's checkout (`store.conn().await?`) is not retried.
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match Connection::connect_with_config(self.config.clone()).await {
                Ok(conn) => return Ok(CountedConn::new(conn)),
                Err(e) if attempt < 4 && is_transient_connect(&e) => {
                    tokio::time::sleep(Duration::from_millis(150 * u64::from(attempt))).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn recycle(&self, conn: &mut CountedConn, _: &Metrics) -> RecycleResult<Error> {
        // Discard a connection desynced by a cancelled query (issue #11) — it
        // would otherwise hang the next caller.
        if conn.poisoned.load(Relaxed) {
            return Err(RecycleError::message(
                "connection poisoned by a cancelled query",
            ));
        }
        if conn.is_closed() {
            return Err(RecycleError::message("connection closed"));
        }
        // Retire before the leaked cursors reach open_cursors. Returning Err makes
        // deadpool drop the connection (tearing down the TNS session, which frees
        // every cursor) and create a fresh one on the next checkout.
        if conn.execs() >= self.max_execs {
            return Err(RecycleError::message(
                "retiring connection at cursor-leak threshold",
            ));
        }
        // Clean any pending transaction, then verify liveness so a connection the
        // server dropped (e.g. under CI resource pressure) is replaced rather than
        // handed to the next caller. The ping is itself a counted statement.
        conn.rollback().await.ok();
        conn.query("SELECT 1 FROM dual", &[])
            .await
            .map_err(RecycleError::Backend)?;
        Ok(())
    }
}

/// The Oracle connection pool (deadpool over [`CountingManager`]).
pub type Pool = managed::Pool<CountingManager>;

/// A pooled connection handle; derefs to [`CountedConn`].
pub type Object = managed::Object<CountingManager>;

/// Build a pool of at most `max_size` connections, each retired after `max_execs`
/// statements. Connections are established lazily on first use.
///
/// # Errors
///
/// Returns a [`BuildError`] if the pool cannot be constructed.
pub fn build(config: Config, max_size: usize, max_execs: usize) -> Result<Pool, BuildError> {
    Pool::builder(CountingManager {
        config,
        max_execs: max_execs.max(1),
    })
    .max_size(max_size.max(1))
    .runtime(deadpool::Runtime::Tokio1)
    .timeouts(managed::Timeouts {
        wait: Some(Duration::from_secs(30)),
        create: Some(Duration::from_secs(30)),
        recycle: Some(Duration::from_secs(5)),
    })
    .build()
}
