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

/// Skip the liveness ping on checkout when a connection was used within this
/// window — it is certainly still alive, and the ping costs a round-trip plus a
/// leaked cursor every time. Idle connections (beyond this) are still pinged so a
/// stale one is detected and replaced.
const LIVENESS_BYPASS_WINDOW: Duration = Duration::from_secs(30);

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
        let conn = Connection::connect_with_config(self.config.clone()).await?;
        Ok(CountedConn::new(conn))
    }

    async fn recycle(&self, conn: &mut CountedConn, metrics: &Metrics) -> RecycleResult<Error> {
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
        // Clean any pending transaction (cheap no-op when there is none).
        conn.rollback().await.ok();
        // Liveness check only for connections idle beyond the keepalive window:
        // under load a connection is re-borrowed within milliseconds and is
        // certainly alive, so skip the `SELECT 1 FROM dual` — it is a round-trip
        // *and* a leaking cursor on every checkout. Mirrors HikariCP's
        // `aliveBypassWindow`.
        if metrics.last_used() > LIVENESS_BYPASS_WINDOW {
            conn.query("SELECT 1 FROM dual", &[])
                .await
                .map_err(RecycleError::Backend)?;
        }
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
