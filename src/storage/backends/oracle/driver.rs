//! Shared bind/decode helpers for the (blocking) `oracle` ODPI-C driver.
//!
//! The `oracle` crate (kubo/rust-oracle, ODPI-C / Instant Client) is a mature,
//! thick driver: it closes server cursors on `Statement` drop (no cursor leak),
//! returns whole result sets with no row cap, decodes `NUMBER` straight to
//! `i64`/`i32`, and reads CLOBs inline as `String`. So this module is now small:
//! the [`OraBind`] parameter enum (uniform `&[&dyn ToSql]` bindings, with an
//! explicit-CLOB variant for the large schema-text columns), the `:N` constructor
//! helpers, error mapping/classification, and the window-clause builder.
//!
//! ## Dialect notes
//!
//! The query layer is a hand-written Oracle translation of the `PostgreSQL`
//! statements in the sibling modules. Conventions used throughout:
//!
//! * **All per-execution values are bound** as `:1`, `:2`, … (each a distinct
//!   placeholder — a value needed twice gets two placeholders so binds never
//!   collide). Booleans are lowered to the integers `0`/`1` so the dialect SQL
//!   compares them as `:N = 1`, matching the `NUMBER(1)` storage of the boolean
//!   columns. Only table/column **identifiers** may be `format!`-inlined.
//! * The two large text columns (`schema_text`, `canonical_form`) are bound with
//!   [`clob`] ([`OraBind::Clob`]), which forces an explicit `OracleType::CLOB`
//!   bind so values larger than ~32 KB write correctly (a plain string bind would
//!   default to `NVARCHAR2` and fail with ORA-01461 / ORA-22835).
//! * `now()` → `SYSTIMESTAMP`; `ON CONFLICT` → `UPDATE`-then-`INSERT` with a
//!   unique-violation retry; `DISTINCT ON` → `ROW_NUMBER()`; `^@` → `INSTR(..)=1`;
//!   `SELECT EXISTS(..)` → `SELECT CASE WHEN EXISTS(..) THEN 1 ELSE 0 END FROM dual`.
//! * The registry-mode column is named `registry_mode` here (it is `mode` on
//!   Postgres) because `MODE` is an Oracle reserved word.

use oracle::Connection;
use oracle::SqlValue;
use oracle::sql_type::{OracleType, ToSql};

use crate::error::KoraError;

/// Embedded Oracle migration (idempotent PL/SQL block).
pub(super) const MIGRATION_001: &str =
    include_str!("../../../../migrations/oracle/001_initial_schema.sql");

/// Columns and joins selected for every [`SchemaVersion`] lookup, in the fixed
/// order consumed by the row mappers (`id, subject, version, schema_type,
/// schema_text`). `schema_text` is a CLOB read inline as a `String`.
///
/// [`SchemaVersion`]: crate::storage::types::SchemaVersion
pub(super) const SV_COLS: &str = "sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text";
pub(super) const SV_JOIN: &str = "FROM schema_versions sv \
     JOIN subjects sub ON sv.subject_id = sub.id \
     JOIN schema_contents sc ON sv.content_id = sc.id";

/// A value bound into a parameterized query.
///
/// All variants implement [`ToSql`] so a `&[OraBind]` lowers to the
/// `&[&dyn ToSql]` the driver's `query`/`execute` expect (via [`to_refs`]).
/// [`OraBind::Clob`] forces an explicit `OracleType::CLOB` bind type so large
/// schema-text values write correctly; the others use the driver's default
/// mapping (`String`/`&str` → `NVARCHAR2`, `i64` → `NUMBER`).
pub(super) enum OraBind {
    /// A text value (`NVARCHAR2`).
    Str(String),
    /// A signed integer (`NUMBER`); booleans are lowered to `0`/`1` here.
    Int(i64),
    /// A large text value bound explicitly as `CLOB` (`schema_text` /
    /// `canonical_form`), so values larger than ~32 KB write correctly.
    Clob(String),
}

impl ToSql for OraBind {
    fn oratype(&self, conn: &Connection) -> oracle::Result<OracleType> {
        match self {
            Self::Str(s) => s.oratype(conn),
            Self::Int(n) => n.oratype(conn),
            Self::Clob(_) => Ok(OracleType::CLOB),
        }
    }

    fn to_sql(&self, val: &mut SqlValue) -> oracle::Result<()> {
        match self {
            Self::Str(s) | Self::Clob(s) => s.to_sql(val),
            Self::Int(n) => n.to_sql(val),
        }
    }
}

/// Bind a string value (`:N`, `NVARCHAR2`).
pub(super) fn s(v: &str) -> OraBind {
    OraBind::Str(v.to_owned())
}

/// Bind an integer value (`:N`, `NUMBER`).
///
/// Per-execution integers (ids, versions) must be **bound**, not inlined into the
/// SQL text: an inlined literal produces a distinct SQL string per value, forcing
/// a hard parse on every call. Binding keeps the SQL text constant so the
/// statement parses once and its cursor is shared.
pub(super) fn i(v: i64) -> OraBind {
    OraBind::Int(v)
}

/// Bind a boolean value, lowered to the integer `0`/`1` (`NUMBER(1)`).
///
/// A native `bool` bind would map to `OracleType::Boolean`, which Oracle accepts
/// only in PL/SQL — never as a `NUMBER(1)` column comparison — so booleans are
/// always lowered to integers here.
pub(super) fn b(v: bool) -> OraBind {
    OraBind::Int(i64::from(v))
}

/// Bind a large text value explicitly as `CLOB` (`schema_text` / `canonical_form`).
pub(super) fn clob(v: &str) -> OraBind {
    OraBind::Clob(v.to_owned())
}

/// Borrow a slice of [`OraBind`]s as the `&[&dyn ToSql]` the driver expects.
///
/// The returned `Vec` borrows from `binds`, so `binds` must outlive it (the
/// caller keeps both in scope for the duration of the `query`/`execute` call).
pub(super) fn to_refs(binds: &[OraBind]) -> Vec<&dyn ToSql> {
    binds.iter().map(|x| x as &dyn ToSql).collect()
}

/// Map a driver error to [`KoraError::BackendDataStore`].
///
/// Takes the error by value so it can be used directly as `.map_err(map_ora)`.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn map_ora(e: oracle::Error) -> KoraError {
    KoraError::BackendDataStore(e.to_string())
}

/// True when the error is ORA-00001 (unique constraint violated).
pub(super) fn is_unique_violation(e: &oracle::Error) -> bool {
    e.oci_code() == Some(1)
}

/// True for transient listener/handoff errors seen while establishing connections,
/// especially against Oracle Free under bursty connection creation. These are safe
/// to retry: the listener is momentarily unable to hand off or has not finished
/// registering the service.
///   ORA-12516/12518/12520 — listener could not hand off / no handler available
///   ORA-12514/12564       — service not (yet) registered / connection refused
pub(super) fn is_transient_connect(e: &oracle::Error) -> bool {
    matches!(e.oci_code(), Some(12516 | 12518 | 12520 | 12514 | 12564))
}

/// Escape LIKE metacharacters and append `%`, mirroring the `PostgreSQL` layer.
pub(super) fn like_pattern(prefix: Option<&str>) -> Option<String> {
    prefix.filter(|p| !p.is_empty()).map(|p| {
        let escaped = p
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("{escaped}%")
    })
}

/// Append an `OFFSET … ROWS [FETCH NEXT … ROWS ONLY]` window to an ordered SELECT.
///
/// `base_sql` must be a complete, ordered SELECT **without** a trailing
/// `OFFSET/FETCH` clause (a deterministic `ORDER BY` is required for stable
/// paging). `limit < 0` means unbounded (offset only). Negative offsets are
/// clamped to `0`.
pub(super) fn append_window(base_sql: &str, offset: i64, limit: i64) -> String {
    let off = offset.max(0);
    if limit < 0 {
        format!("{base_sql} OFFSET {off} ROWS")
    } else {
        format!("{base_sql} OFFSET {off} ROWS FETCH NEXT {limit} ROWS ONLY")
    }
}

// -- Transaction / query helpers --
//
// These centralize the patterns every per-domain module would otherwise repeat:
// commit-or-rollback for transactional writes, and "first row, column 0" scalar
// reads. The per-domain modules keep their dialect SQL verbatim and call these
// for the mechanical parts.

/// Commit on `Ok`, roll back on `Err`, then return the result unchanged.
///
/// Every transactional operation must wrap its body in this: the native OCI pool
/// does not roll back a dirty connection when it is returned (unlike a recycling
/// pool), so a leaked pending transaction would be inherited by the next borrower.
pub(super) fn commit_or_rollback<T>(
    conn: &Connection,
    result: Result<T, KoraError>,
) -> Result<T, KoraError> {
    match result {
        Ok(v) => {
            conn.commit().map_err(map_ora)?;
            Ok(v)
        }
        Err(e) => {
            conn.rollback().ok();
            Err(e)
        }
    }
}

/// Run a query and return its first [`Row`], or `None` when the result is empty.
///
/// [`Row`]: oracle::Row
pub(super) fn first_row(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<Option<oracle::Row>, KoraError> {
    let refs = to_refs(binds);
    conn.query(sql, &refs)
        .map_err(map_ora)?
        .next()
        .transpose()
        .map_err(map_ora)
}

/// First column of the first row decoded as `T`, or `None` when the result is
/// empty (the column value itself may also be SQL `NULL` if `T` is an `Option`).
pub(super) fn scalar_opt<T>(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<Option<T>, KoraError>
where
    T: oracle::sql_type::FromSql,
{
    first_row(conn, sql, binds)?
        .map(|row| row.get::<usize, T>(0))
        .transpose()
        .map_err(map_ora)
}

/// First column of the first row as text, erroring if there is no row. Use for
/// `COALESCE`d queries that always return exactly one non-null row.
pub(super) fn scalar_string(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<String, KoraError> {
    scalar_opt::<String>(conn, sql, binds)?
        .ok_or_else(|| KoraError::BackendDataStore("expected exactly one row".to_owned()))
}

/// First column of the first row as text, or `None` when absent/NULL.
pub(super) fn scalar_opt_string(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<Option<String>, KoraError> {
    scalar_opt::<Option<String>>(conn, sql, binds).map(Option::flatten)
}

/// Read a `0`/`1` existence query (`CASE WHEN EXISTS … FROM dual`) as a bool;
/// `false` when there is no row.
pub(super) fn scalar_bool(
    conn: &Connection,
    sql: &str,
    binds: &[OraBind],
) -> Result<bool, KoraError> {
    Ok(scalar_opt::<i64>(conn, sql, binds)? == Some(1))
}
