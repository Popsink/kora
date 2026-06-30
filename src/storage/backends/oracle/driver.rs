//! `oracle_rs` driver workarounds and the shared decode machinery.
//!
//! This module isolates everything that exists only because the pure-Rust
//! `oracle_rs` 0.1 driver is young: the positional [`SqlRow`] adapter
//! ([`OraRow`]), bind lowering ([`lower`]), the four read-path workarounds
//! ([`query_all`], [`fetch_schema_texts`], the CLOB cell readers, and the
//! error classifiers), and the `SchemaVersion` row mappers. The thin adapter
//! (`super`) and the per-domain SQL modules call into these.
//!
//! ## Working around `oracle_rs` 0.1 limitations
//!
//! The pure-Rust driver is young, and three quirks shape the read path here:
//!
//! * **Row cap per fetch.** `query` returns at most ~100 rows and its
//!   cursor-continuation is unusable (`has_more_rows` is an unreliable
//!   false-negative and `fetch_more` returns nothing), so multi-row reads page
//!   with independent `OFFSET … FETCH NEXT …` queries — see [`query_all`].
//! * **Multi-row CLOB decode.** Selecting a CLOB across a large multi-row
//!   response mis-decodes ("buffer underflow"), so `schema_text` is excluded
//!   from multi-row metadata queries ([`SV_COLS_META`]) and read in adaptive,
//!   self-splitting batches ([`fetch_schema_texts`]); single-row reads embed it.
//! * **Cursor leak.** Each `query` (SELECT) leaks a server cursor for the
//!   connection's lifetime, so a single connection can serve only `open_cursors`
//!   (typically 300+) SELECTs before the server drops it. The batching above
//!   keeps any one request's query count low; deadpool transparently replaces a
//!   dropped connection. Very large unbounded listings on one connection remain
//!   bounded by `open_cursors` — Postgres stays the backend for heavy fleets.

use std::collections::HashMap;

use oracle_rs::types::LobValue;
use oracle_rs::{Connection, QueryResult, Row, Value};

use crate::error::KoraError;
use crate::storage::sql::{Bind, Row as SqlRow};
use crate::storage::types::SchemaVersion;

/// Embedded Oracle migration (idempotent PL/SQL block).
pub(super) const MIGRATION_001: &str =
    include_str!("../../../../migrations_oracle/001_initial_schema.sql");

/// Columns and joins selected for every [`SchemaVersion`] lookup, in the fixed
/// order consumed by [`row_to_sv`].
pub(super) const SV_COLS: &str = "sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text";
pub(super) const SV_JOIN: &str = "FROM schema_versions sv \
     JOIN subjects sub ON sv.subject_id = sub.id \
     JOIN schema_contents sc ON sv.content_id = sc.id";

/// Metadata columns (no CLOB) for **multi-row** lookups. The `oracle-rs` driver
/// mis-decodes a CLOB column inside a multi-row `SELECT` result, so multi-row
/// reads omit `schema_text` and fetch the CLOBs separately in batches via
/// [`fetch_schema_texts`]; single-row reads ([`SV_COLS`]) decode the CLOB inline.
pub(super) const SV_COLS_META: &str = "sc.id, sub.name, sv.version, sc.schema_type";

/// Lower neutral [`Bind`]s to bound `oracle_rs` [`Value`]s.
///
/// Every parameter — strings, integers, and booleans — becomes a bound `:N`
/// variable (no inlining on the toolkit path). Booleans map to the integers
/// `0`/`1`, so the dialect SQL compares them as `:N = 1`, matching the
/// `NUMBER(1)` storage of the boolean columns.
pub(super) fn lower(params: &[Bind]) -> Vec<Value> {
    params
        .iter()
        .map(|b| match b {
            Bind::Str(s) => Value::from(s.as_str()),
            Bind::I64(i) => Value::from(*i),
            Bind::Bool(v) => Value::from(i64::from(*v)),
        })
        .collect()
}

/// Wraps an `oracle_rs` [`Row`] so it can be decoded positionally through the
/// backend-neutral [`SqlRow`] trait, reusing the file's decode helpers (which
/// absorb Oracle's `NUMBER`-as-decimal-string and `NUMBER(1)`-boolean quirks).
pub struct OraRow(pub(super) Row);

impl SqlRow for OraRow {
    fn get_i64(&self, idx: usize) -> Result<i64, KoraError> {
        val_i64(self.0.get(idx))
            .ok_or_else(|| KoraError::BackendDataStore(format!("expected integer at column {idx}")))
    }

    fn get_i32(&self, idx: usize) -> Result<i32, KoraError> {
        i32::try_from(self.get_i64(idx)?)
            .map_err(|_| KoraError::BackendDataStore("integer out of range".to_owned()))
    }

    fn get_str(&self, idx: usize) -> Result<String, KoraError> {
        // SYNC: handles only non-CLOB / inline values (the only thing the shared
        // helpers ever ask for; CLOB schema_text is read elsewhere via cell_text).
        match self.0.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(Value::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
            Some(Value::Lob(LobValue::Inline(b))) => Ok(String::from_utf8_lossy(b).into_owned()),
            Some(Value::Lob(LobValue::Locator(_))) => Err(KoraError::BackendDataStore(
                "clob locator requires async read".to_owned(),
            )),
            None | Some(Value::Null) => Ok(String::new()),
            Some(other) => Ok(other.to_string()),
        }
    }

    fn get_bool(&self, idx: usize) -> Result<bool, KoraError> {
        Ok(val_i64(self.0.get(idx)) == Some(1))
    }

    fn get_opt_i64(&self, idx: usize) -> Result<Option<i64>, KoraError> {
        Ok(val_i64(self.0.get(idx)))
    }

    fn get_opt_str(&self, idx: usize) -> Result<Option<String>, KoraError> {
        match self.0.get(idx) {
            None | Some(Value::Null) => Ok(None),
            _ => Ok(Some(self.get_str(idx)?)),
        }
    }
}

// -- Value / row helpers --

/// True when the error is ORA-00001 (unique constraint violated).
pub(super) fn is_unique_violation(e: &oracle_rs::Error) -> bool {
    e.to_string().contains("ORA-00001")
}

/// Bind a string value.
pub(super) fn s(v: &str) -> Value {
    Value::from(v)
}

/// Extract an `i64` from a column value.
///
/// The `oracle-rs` driver returns Oracle `NUMBER` columns (including identity
/// ids, `COUNT(*)`, and `CASE … THEN 1 ELSE 0` predicates) as decimal **strings**
/// to preserve precision, so fall back to parsing a string value.
pub(super) fn val_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(text) => text.trim().parse::<i64>().ok(),
        other => other.as_i64(),
    }
}

/// Extract a text column, transparently reading CLOBs (inline or via locator).
pub(super) async fn cell_text(conn: &Connection, v: Option<&Value>) -> Result<String, KoraError> {
    match v {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        Some(Value::Lob(LobValue::Inline(b))) => Ok(String::from_utf8_lossy(b).into_owned()),
        Some(Value::Lob(LobValue::Locator(loc))) => Ok(conn.read_clob(loc).await?),
        None | Some(Value::Null | Value::Lob(_)) => Ok(String::new()),
        Some(other) => Ok(other.to_string()),
    }
}

/// Extract a required integer column.
pub(super) fn cell_i64(row: &Row, idx: usize) -> Result<i64, KoraError> {
    val_i64(row.get(idx))
        .ok_or_else(|| KoraError::BackendDataStore(format!("expected integer at column {idx}")))
}

/// Extract a required `i32` column (version numbers).
pub(super) fn cell_i32(row: &Row, idx: usize) -> Result<i32, KoraError> {
    i32::try_from(cell_i64(row, idx)?)
        .map_err(|_| KoraError::BackendDataStore("integer out of range".to_owned()))
}

/// Map a row selecting [`SV_COLS`] (in order) to a [`SchemaVersion`].
pub(super) async fn row_to_sv(conn: &Connection, row: &Row) -> Result<SchemaVersion, KoraError> {
    Ok(SchemaVersion {
        id: cell_i64(row, 0)?,
        subject: cell_text(conn, row.get(1)).await?,
        version: cell_i32(row, 2)?,
        schema_type: cell_text(conn, row.get(3)).await?,
        schema: cell_text(conn, row.get(4)).await?,
        references: Vec::new(),
    })
}

/// Collect rows selecting [`SV_COLS_META`] (4 columns, no CLOB) as
/// [`SchemaVersion`]s, fetching their `schema_text` CLOBs in batches.
pub(super) async fn collect_svs(
    conn: &Connection,
    result: &QueryResult,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let ids = result
        .iter()
        .map(|row| cell_i64(row, 0))
        .collect::<Result<Vec<_>, _>>()?;
    let texts = fetch_schema_texts(conn, &ids).await?;
    let mut out = Vec::with_capacity(result.row_count());
    for row in result.iter() {
        let id = cell_i64(row, 0)?;
        out.push(SchemaVersion {
            id,
            subject: cell_text(conn, row.get(1)).await?,
            version: cell_i32(row, 2)?,
            schema_type: cell_text(conn, row.get(3)).await?,
            schema: texts.get(&id).cloned().unwrap_or_default(),
            references: Vec::new(),
        });
    }
    Ok(out)
}

/// Optimistic batch size for [`fetch_schema_texts`]. The driver mis-decodes a
/// CLOB across a large multi-row response ("buffer underflow"), a fault that
/// grows with the total bytes in the batch, so this stays well below where it
/// trips for typical schemas; oversized batches are split adaptively.
const CLOB_BATCH: usize = 50;

/// True for the driver's multi-row CLOB decode fault, which is recoverable by
/// reading fewer rows at a time.
pub(super) fn is_buffer_underflow(e: &oracle_rs::Error) -> bool {
    e.to_string().contains("buffer underflow")
}

/// Fetch `schema_text` for many content ids at once, returning an id → text map.
///
/// The schema text is a CLOB, and the driver has two opposing limitations:
/// reading one row per query leaks a server cursor each call (the session dies
/// after a few hundred), while selecting the CLOB across a large multi-row
/// response mis-decodes ("buffer underflow"). So ids are read in `IN (...)`
/// batches — few queries — and any batch that trips the decode fault is split
/// in half and retried, down to a single row, which always decodes correctly.
pub(super) async fn fetch_schema_texts(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, String>, KoraError> {
    let mut map = HashMap::with_capacity(ids.len());
    // Worklist of id slices to read; an oversized batch is replaced by its two
    // halves. Pushed in reverse so the first chunk is processed first.
    let mut todo: Vec<&[i64]> = ids.chunks(CLOB_BATCH).rev().collect();
    while let Some(slice) = todo.pop() {
        let in_list = slice
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        match conn
            .query(
                &format!("SELECT id, schema_text FROM schema_contents WHERE id IN ({in_list})"),
                &[],
            )
            .await
        {
            Ok(result) => {
                for row in result.iter() {
                    let id = cell_i64(row, 0)?;
                    map.insert(id, cell_text(conn, row.get(1)).await?);
                }
            }
            Err(e) if slice.len() > 1 && is_buffer_underflow(&e) => {
                let mid = slice.len() / 2;
                todo.push(&slice[mid..]);
                todo.push(&slice[..mid]);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(map)
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

/// The driver's hard-coded fetch/prefetch size: a single `query` round-trip
/// returns at most this many rows (see `oracle_rs` `execute_query_with_params`).
const ORACLE_FETCH_SIZE: usize = 100;

/// Run a SELECT and return the **full** result for the requested window.
///
/// `oracle-rs` returns at most [`ORACLE_FETCH_SIZE`] rows per round-trip and its
/// cursor-continuation (`fetch_more`) is unusable in this version: it reports
/// `has_more_rows = false` even with rows pending and returns nothing on the
/// next fetch. So instead of draining one cursor, this pages with independent
/// `OFFSET … FETCH NEXT …` queries — each a fresh statement capped at the fetch
/// size — until a short page signals the end (or `limit` is satisfied).
///
/// `base_sql` must be a complete, ordered SELECT **without** a trailing
/// `OFFSET/FETCH` clause (a deterministic `ORDER BY` is required for stable
/// paging). `limit < 0` means unbounded. The aggregated rows are returned as a
/// single [`QueryResult`] so callers consume them exactly as a normal query.
pub(super) async fn query_all(
    conn: &Connection,
    base_sql: &str,
    params: &[Value],
    offset: i64,
    limit: i64,
) -> Result<QueryResult, KoraError> {
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let mut page_offset = offset.max(0);
    loop {
        // Rows still wanted this page; the driver caps any fetch at the size.
        let want = if limit < 0 {
            ORACLE_FETCH_SIZE
        } else {
            let remaining = limit - i64::try_from(rows.len()).unwrap_or(i64::MAX);
            if remaining <= 0 {
                break;
            }
            let cap = i64::try_from(ORACLE_FETCH_SIZE).unwrap_or(i64::MAX);
            usize::try_from(remaining.min(cap)).unwrap_or(ORACLE_FETCH_SIZE)
        };
        let sql = format!("{base_sql} OFFSET {page_offset} ROWS FETCH NEXT {want} ROWS ONLY");
        let mut page = conn.query(&sql, params).await?;
        if columns.is_empty() {
            columns = page.columns.clone();
        }
        let got = page.row_count();
        rows.append(&mut page.rows);
        if got < want {
            break; // short page → no more rows
        }
        page_offset += i64::try_from(got).unwrap_or(0);
    }
    Ok(QueryResult {
        columns,
        rows,
        rows_affected: 0,
        has_more_rows: false,
        cursor_id: 0,
    })
}
