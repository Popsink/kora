//! Oracle storage backend (behind the `oracle` cargo feature).
//!
//! Uses the **pure-Rust** [`oracle_rs`] driver (Oracle TNS/TTC protocol, no OCI /
//! ODPI-C / Instant Client) with a [`deadpool_oracle`] connection pool. The whole
//! backend is async-native — no `spawn_blocking` bridge — and an Oracle-enabled
//! build remains a single self-contained binary that connects over TCP, exactly
//! like the Postgres path.
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

use async_trait::async_trait;
use deadpool_oracle::{Object, Pool, PoolBuilder};
use oracle_rs::types::LobValue;
use oracle_rs::{Config, Connection, QueryResult, Row, Value};

use super::schemas::{CompatCheck, NewSchema, SchemaVersion, SubjectVersion};
use super::subjects::HardDeleteResult;
use super::{PoolStats, Storage};
use crate::error::KoraError;
use crate::schema::{self, SchemaFormat};
use crate::types::SchemaReference;

/// Embedded Oracle migration (idempotent PL/SQL block).
const MIGRATION_001: &str = include_str!("../../migrations_oracle/001_initial_schema.sql");

/// Columns and joins selected for every [`SchemaVersion`] lookup, in the fixed
/// order consumed by [`row_to_sv`].
const SV_COLS: &str = "sc.id, sub.name, sv.version, sc.schema_type, sc.schema_text";
const SV_JOIN: &str = "FROM schema_versions sv \
     JOIN subjects sub ON sv.subject_id = sub.id \
     JOIN schema_contents sc ON sv.content_id = sc.id";

/// Metadata columns (no CLOB) for **multi-row** lookups. The `oracle-rs` driver
/// mis-decodes a CLOB column inside a multi-row result set ("buffer underflow"),
/// so the `schema_text` CLOB is fetched per row in a single-row query
/// ([`fetch_schema_text`]); single-row CLOB fetches decode correctly.
const SV_COLS_META: &str = "sc.id, sub.name, sv.version, sc.schema_type";

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
    async fn conn(&self) -> Result<Object, KoraError> {
        self.pool
            .get()
            .await
            .map_err(|e| KoraError::BackendDataStore(e.to_string()))
    }
}

// -- Value / row helpers --

/// True when the error is ORA-00001 (unique constraint violated).
fn is_unique_violation(e: &oracle_rs::Error) -> bool {
    e.to_string().contains("ORA-00001")
}

/// Bind a string value.
fn s(v: &str) -> Value {
    Value::from(v)
}

/// Extract an `i64` from a column value.
///
/// The `oracle-rs` driver returns Oracle `NUMBER` columns (including identity
/// ids, `COUNT(*)`, and `CASE … THEN 1 ELSE 0` predicates) as decimal **strings**
/// to preserve precision, so fall back to parsing a string value.
fn val_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(text) => text.trim().parse::<i64>().ok(),
        other => other.as_i64(),
    }
}

/// Extract a text column, transparently reading CLOBs (inline or via locator).
async fn cell_text(conn: &Connection, v: Option<&Value>) -> Result<String, KoraError> {
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
fn cell_i64(row: &Row, idx: usize) -> Result<i64, KoraError> {
    val_i64(row.get(idx))
        .ok_or_else(|| KoraError::BackendDataStore(format!("expected integer at column {idx}")))
}

/// Extract a required `i32` column (version numbers).
fn cell_i32(row: &Row, idx: usize) -> Result<i32, KoraError> {
    i32::try_from(cell_i64(row, idx)?)
        .map_err(|_| KoraError::BackendDataStore("integer out of range".to_owned()))
}

/// Map a row selecting [`SV_COLS`] (in order) to a [`SchemaVersion`].
async fn row_to_sv(conn: &Connection, row: &Row) -> Result<SchemaVersion, KoraError> {
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
/// [`SchemaVersion`]s, fetching each `schema_text` separately.
async fn collect_svs(
    conn: &Connection,
    result: &QueryResult,
) -> Result<Vec<SchemaVersion>, KoraError> {
    let mut out = Vec::with_capacity(result.row_count());
    for row in result.iter() {
        let id = cell_i64(row, 0)?;
        out.push(SchemaVersion {
            id,
            subject: cell_text(conn, row.get(1)).await?,
            version: cell_i32(row, 2)?,
            schema_type: cell_text(conn, row.get(3)).await?,
            schema: fetch_schema_text(conn, id).await?,
            references: Vec::new(),
        });
    }
    Ok(out)
}

/// Fetch a single schema's text by content id (single-row CLOB read).
///
/// The id is **bound** (not inlined) because this runs once per row of a listing:
/// inlining would make every iteration a distinct statement, exhausting the
/// session's open cursors. Binding reuses a single cached statement.
async fn fetch_schema_text(conn: &Connection, id: i64) -> Result<String, KoraError> {
    let result = conn
        .query(
            "SELECT schema_text FROM schema_contents WHERE id = :1",
            &[Value::from(id)],
        )
        .await?;
    match result.first() {
        Some(row) => cell_text(conn, row.get(0)).await,
        None => Ok(String::new()),
    }
}

/// `OFFSET .. ROWS [FETCH NEXT .. ROWS ONLY]` clause. `limit < 0` means no limit.
fn page_clause(offset: i64, limit: i64) -> String {
    if limit >= 0 {
        format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    } else {
        format!(" OFFSET {offset} ROWS")
    }
}

/// Escape LIKE metacharacters and append `%`, mirroring the `PostgreSQL` layer.
fn like_pattern(prefix: Option<&str>) -> Option<String> {
    prefix.filter(|p| !p.is_empty()).map(|p| {
        let escaped = p
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("{escaped}%")
    })
}

#[async_trait]
impl Storage for OracleStorage {
    // -- Lifecycle --

    async fn migrate(&self) -> Result<(), KoraError> {
        let conn = self.conn().await?;
        conn.execute_plsql(MIGRATION_001, &[]).await?;
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
        let like = like_pattern(prefix);
        let filter = if deleted_only {
            "deleted = 1"
        } else if include_deleted {
            "1 = 1"
        } else {
            "deleted = 0"
        };
        let like_sql = if like.is_some() {
            " AND name LIKE :1 ESCAPE '\\'"
        } else {
            ""
        };
        let sql = format!(
            "SELECT name FROM subjects WHERE {filter}{like_sql} ORDER BY name{}",
            page_clause(offset, limit)
        );
        let params: Vec<Value> = like.iter().map(|p| s(p)).collect();

        let conn = self.conn().await?;
        let result = conn.query(&sql, &params).await?;
        Ok(result
            .iter()
            .filter_map(|row| row.get_string(0).map(str::to_owned))
            .collect())
    }

    async fn soft_delete_subject(&self, name: &str) -> Result<Vec<i32>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT sv.version FROM schema_versions sv \
                 WHERE sv.subject_id = (SELECT id FROM subjects WHERE name = :1) AND sv.deleted = 0 \
                 ORDER BY sv.version",
                &[s(name)],
            )
            .await?;
        let mut versions = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            versions.push(cell_i32(row, 0)?);
        }
        conn.execute_dml_sql(
            "UPDATE schema_versions SET deleted = 1 \
             WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) AND deleted = 0",
            &[s(name)],
        )
        .await?;
        conn.execute_dml_sql(
            "UPDATE subjects SET deleted = 1 WHERE name = :1 AND deleted = 0",
            &[s(name)],
        )
        .await?;
        conn.commit().await?;
        versions.sort_unstable();
        Ok(versions)
    }

    async fn hard_delete_subject(&self, name: &str) -> Result<HardDeleteResult, KoraError> {
        let conn = self.conn().await?;
        let found = conn
            .query(
                "SELECT id, deleted FROM subjects WHERE name = :1 FOR UPDATE",
                &[s(name)],
            )
            .await?;
        let Some(row) = found.first() else {
            return Ok(HardDeleteResult::NotFound);
        };
        let subject_id = cell_i64(row, 0)?;
        let deleted = cell_i64(row, 1)?;
        if deleted == 0 {
            conn.rollback().await?;
            return Ok(HardDeleteResult::NotSoftDeleted);
        }

        let vresult = conn
            .query(
                &format!(
                    "SELECT version FROM schema_versions \
                     WHERE subject_id = {subject_id} AND deleted = 1"
                ),
                &[],
            )
            .await?;
        let mut versions = Vec::with_capacity(vresult.row_count());
        for row in vresult.iter() {
            versions.push(cell_i32(row, 0)?);
        }

        for v in &versions {
            let referenced = conn
                .query(
                    &format!(
                        "SELECT CASE WHEN EXISTS (\
                            SELECT 1 FROM schema_references sr \
                            JOIN schema_versions sv ON sr.content_id = sv.content_id \
                            WHERE sr.subject = :1 AND sr.version = {v} AND sv.deleted = 0\
                         ) THEN 1 ELSE 0 END FROM dual"
                    ),
                    &[s(name)],
                )
                .await?;
            if referenced.first().and_then(|r| val_i64(r.get(0))) == Some(1) {
                conn.rollback().await?;
                return Ok(HardDeleteResult::ReferenceExists(format!(
                    "{name} version {v}"
                )));
            }
        }

        conn.execute_dml_sql(
            &format!("DELETE FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 1"),
            &[],
        )
        .await?;

        let active = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_versions WHERE subject_id = {subject_id} AND deleted = 0\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &[],
            )
            .await?;
        if active.first().and_then(|r| val_i64(r.get(0))) != Some(1) {
            conn.execute_dml_sql(
                &format!("DELETE FROM subjects WHERE id = {subject_id}"),
                &[],
            )
            .await?;
        }

        conn.commit().await?;
        versions.sort_unstable();
        Ok(HardDeleteResult::Deleted(versions))
    }

    async fn find_subject_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>, KoraError> {
        let filter = if include_deleted {
            ""
        } else {
            " AND deleted = 0"
        };
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!("SELECT id FROM subjects WHERE name = :1{filter}"),
                &[s(name)],
            )
            .await?;
        Ok(result.first().and_then(|row| val_i64(row.get(0))))
    }

    async fn subject_exists(&self, name: &str, include_deleted: bool) -> Result<bool, KoraError> {
        let filter = if include_deleted {
            ""
        } else {
            " AND deleted = 0"
        };
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS \
                     (SELECT 1 FROM subjects WHERE name = :1{filter}) THEN 1 ELSE 0 END FROM dual"
                ),
                &[s(name)],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
    }

    async fn subject_is_soft_deleted(&self, name: &str) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT CASE WHEN EXISTS \
                 (SELECT 1 FROM subjects WHERE name = :1 AND deleted = 1) THEN 1 ELSE 0 END FROM dual",
                &[s(name)],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
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
        let conn = self.conn().await?;

        // Upsert subject and lock it.
        let subject_id = upsert_subject(&conn, subject_name).await?;

        // Per-subject idempotency: existing active version with this fingerprint?
        let (fp, fp_col) = if normalize {
            (schema.fingerprint, "fingerprint")
        } else {
            (schema.raw_fingerprint, "raw_fingerprint")
        };
        let existing = conn
            .query(
                &format!(
                    "SELECT sv.content_id, sv.version FROM schema_versions sv \
                     JOIN schema_contents sc ON sv.content_id = sc.id \
                     WHERE sv.subject_id = {subject_id} AND sc.{fp_col} = :1 AND sv.deleted = 0 \
                     ORDER BY sv.version FETCH FIRST 1 ROW ONLY"
                ),
                &[s(fp)],
            )
            .await?;
        if let Some(row) = existing.first() {
            let content_id = cell_i64(row, 0)?;
            let version = cell_i32(row, 1)?;
            conn.commit().await?;
            return Ok((content_id, version, false));
        }

        // Compatibility check inside the transaction (after the subject lock).
        if let Some(compat) = compat {
            run_compat_check(&conn, subject_id, compat).await?;
        }

        let content_id = upsert_content(&conn, schema).await?;

        // Next version under the locked subject, then insert.
        let next = conn
            .query(
                &format!(
                    "SELECT COALESCE(MAX(version), 0) + 1 FROM schema_versions WHERE subject_id = {subject_id}"
                ),
                &[],
            )
            .await?;
        let version = next
            .first()
            .and_then(|r| val_i64(r.get(0)))
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| {
                KoraError::BackendDataStore("could not compute next version".to_owned())
            })?;
        conn.execute_dml_sql(
            &format!(
                "INSERT INTO schema_versions (subject_id, version, content_id) \
                 VALUES ({subject_id}, {version}, {content_id})"
            ),
            &[],
        )
        .await?;

        // Store references only when provided and the content has none yet.
        if !refs.is_empty() {
            let has = conn
                .query(
                    &format!(
                        "SELECT CASE WHEN EXISTS \
                         (SELECT 1 FROM schema_references WHERE content_id = {content_id}) \
                         THEN 1 ELSE 0 END FROM dual"
                    ),
                    &[],
                )
                .await?;
            if has.first().and_then(|r| val_i64(r.get(0))) != Some(1) {
                for r in refs {
                    conn.execute_dml_sql(
                        &format!(
                            "INSERT INTO schema_references (content_id, name, subject, version) \
                             VALUES ({content_id}, :1, :2, {})",
                            r.version
                        ),
                        &[s(&r.name), s(&r.subject)],
                    )
                    .await?;
                }
            }
        }

        conn.commit().await?;
        Ok((content_id, version, true))
    }

    async fn find_all_active_versions(
        &self,
        subject: &str,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT {SV_COLS_META} {SV_JOIN} \
                     WHERE sub.name = :1 AND sv.deleted = 0 ORDER BY sv.version"
                ),
                &[s(subject)],
            )
            .await?;
        collect_svs(&conn, &result).await
    }

    async fn find_schema_by_subject_version(
        &self,
        subject: &str,
        version: i32,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let filter = if include_deleted {
            ""
        } else {
            " AND sv.deleted = 0"
        };
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT {SV_COLS} {SV_JOIN} \
                     WHERE sub.name = :1 AND sv.version = {version}{filter}"
                ),
                &[s(subject)],
            )
            .await?;
        match result.first() {
            Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
            None => Ok(None),
        }
    }

    async fn find_latest_schema_by_subject(
        &self,
        subject: &str,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let filter = if include_deleted {
            ""
        } else {
            " AND sv.deleted = 0"
        };
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT {SV_COLS} {SV_JOIN} \
                     WHERE sub.name = :1{filter} ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY"
                ),
                &[s(subject)],
            )
            .await?;
        match result.first() {
            Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
            None => Ok(None),
        }
    }

    async fn find_schema_by_subject_id_and_fingerprint(
        &self,
        subject_id: i64,
        fingerprint: &str,
        normalize: bool,
        include_deleted: bool,
    ) -> Result<Option<SchemaVersion>, KoraError> {
        let fp_col = if normalize {
            "fingerprint"
        } else {
            "raw_fingerprint"
        };
        let filter = if include_deleted {
            ""
        } else {
            " AND sv.deleted = 0"
        };
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT {SV_COLS} {SV_JOIN} \
                     WHERE sv.subject_id = {subject_id} AND sc.{fp_col} = :1{filter}"
                ),
                &[s(fingerprint)],
            )
            .await?;
        match result.first() {
            Some(row) => Ok(Some(row_to_sv(&conn, row).await?)),
            None => Ok(None),
        }
    }

    async fn find_schema_by_id(&self, id: i64) -> Result<Option<(String, String)>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!("SELECT schema_text, schema_type FROM schema_contents WHERE id = {id}"),
                &[],
            )
            .await?;
        match result.first() {
            Some(row) => {
                let text = cell_text(&conn, row.get(0)).await?;
                let kind = cell_text(&conn, row.get(1)).await?;
                Ok(Some((text, kind)))
            }
            None => Ok(None),
        }
    }

    async fn find_max_schema_id(&self) -> Result<i64, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query("SELECT COALESCE(MAX(id), 0) FROM schema_contents", &[])
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))).unwrap_or(0))
    }

    async fn schema_exists(&self, id: i64) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS \
                     (SELECT 1 FROM schema_contents WHERE id = {id}) THEN 1 ELSE 0 END FROM dual"
                ),
                &[],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
    }

    async fn find_subjects_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, KoraError> {
        let incl = i64::from(include_deleted);
        let filter_sql = if subject_filter.is_some() {
            " AND sub.name = :1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT DISTINCT sub.name FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = {id} AND (sv.deleted = 0 OR {incl} = 1) \
               AND (sub.deleted = 0 OR {incl} = 1){filter_sql} \
             ORDER BY sub.name{}",
            page_clause(offset, limit)
        );
        let params: Vec<Value> = subject_filter.iter().map(|f| s(f)).collect();
        let conn = self.conn().await?;
        let result = conn.query(&sql, &params).await?;
        Ok(result
            .iter()
            .filter_map(|row| row.get_string(0).map(str::to_owned))
            .collect())
    }

    async fn find_versions_by_schema_id(
        &self,
        id: i64,
        include_deleted: bool,
        subject_filter: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SubjectVersion>, KoraError> {
        let incl = i64::from(include_deleted);
        let filter_sql = if subject_filter.is_some() {
            " AND sub.name = :1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT sub.name, sv.version FROM schema_versions sv \
             JOIN subjects sub ON sv.subject_id = sub.id \
             WHERE sv.content_id = {id} AND (sv.deleted = 0 OR {incl} = 1) \
               AND (sub.deleted = 0 OR {incl} = 1){filter_sql} \
             ORDER BY sub.name, sv.version{}",
            page_clause(offset, limit)
        );
        let params: Vec<Value> = subject_filter.iter().map(|f| s(f)).collect();
        let conn = self.conn().await?;
        let result = conn.query(&sql, &params).await?;
        let mut out = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            out.push(SubjectVersion {
                subject: row.get_string(0).unwrap_or_default().to_owned(),
                version: cell_i32(row, 1)?,
            });
        }
        Ok(out)
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
        let page = page_clause(offset, limit);
        let sql = if deleted_only && deleted_as_negative {
            format!(
                "SELECT -sv.version FROM schema_versions sv \
                 JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version{page}"
            )
        } else if deleted_only {
            format!(
                "SELECT sv.version FROM schema_versions sv \
                 JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 AND sv.deleted = 1 ORDER BY sv.version{page}"
            )
        } else if deleted_as_negative && include_deleted {
            format!(
                "SELECT CASE WHEN sv.deleted = 1 THEN -sv.version ELSE sv.version END \
                 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 ORDER BY ABS(sv.version){page}"
            )
        } else {
            let incl = i64::from(include_deleted);
            format!(
                "SELECT sv.version FROM schema_versions sv \
                 JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 AND (sv.deleted = 0 OR {incl} = 1) \
                 ORDER BY sv.version{page}"
            )
        };
        let conn = self.conn().await?;
        let result = conn.query(&sql, &[s(subject)]).await?;
        let mut out = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            out.push(cell_i32(row, 0)?);
        }
        Ok(out)
    }

    async fn list_schemas(
        &self,
        include_deleted: bool,
        latest_only: bool,
        prefix: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<SchemaVersion>, KoraError> {
        let like = like_pattern(prefix);
        let incl = i64::from(include_deleted);
        let like_sql = if like.is_some() {
            " AND sub.name LIKE :1 ESCAPE '\\'"
        } else {
            ""
        };
        let page = page_clause(offset, limit);
        let sql = if latest_only {
            // DISTINCT ON (sub.name) → highest version per subject via ROW_NUMBER.
            format!(
                "SELECT id, subject, version, schema_type FROM (\
                    SELECT sc.id AS id, sub.name AS subject, sv.version AS version, \
                           sc.schema_type AS schema_type, \
                           ROW_NUMBER() OVER (PARTITION BY sub.name ORDER BY sv.version DESC) AS rn \
                    {SV_JOIN} \
                    WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql}\
                 ) WHERE rn = 1 ORDER BY subject{page}"
            )
        } else {
            format!(
                "SELECT {SV_COLS_META} {SV_JOIN} \
                 WHERE (sv.deleted = 0 OR {incl} = 1) AND (sub.deleted = 0 OR {incl} = 1){like_sql} \
                 ORDER BY sub.name, sv.version{page}"
            )
        };
        let params: Vec<Value> = like.iter().map(|p| s(p)).collect();
        let conn = self.conn().await?;
        let result = conn.query(&sql, &params).await?;
        collect_svs(&conn, &result).await
    }

    async fn soft_delete_latest_schema(&self, subject: &str) -> Result<Option<i32>, KoraError> {
        let conn = self.conn().await?;
        let latest = conn
            .query(
                "SELECT sv.version FROM schema_versions sv \
                 JOIN subjects sub ON sv.subject_id = sub.id \
                 WHERE sub.name = :1 AND sv.deleted = 0 \
                 ORDER BY sv.version DESC FETCH FIRST 1 ROW ONLY",
                &[s(subject)],
            )
            .await?;
        let Some(version) = latest.first().map(|r| cell_i32(r, 0)).transpose()? else {
            return Ok(None);
        };
        let updated = conn
            .execute_dml_sql(
                &format!(
                    "UPDATE schema_versions SET deleted = 1 \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 0"
                ),
                &[s(subject)],
            )
            .await?;
        conn.commit().await?;
        Ok((updated >= 1).then_some(version))
    }

    async fn soft_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        let conn = self.conn().await?;
        let updated = conn
            .execute_dml_sql(
                &format!(
                    "UPDATE schema_versions SET deleted = 1 \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 0"
                ),
                &[s(subject)],
            )
            .await?;
        conn.commit().await?;
        Ok((updated >= 1).then_some(version))
    }

    async fn hard_delete_schema_version(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<Option<i32>, KoraError> {
        let conn = self.conn().await?;
        let deleted = conn
            .execute_dml_sql(
                &format!(
                    "DELETE FROM schema_versions \
                     WHERE subject_id = (SELECT id FROM subjects WHERE name = :1) \
                       AND version = {version} AND deleted = 1"
                ),
                &[s(subject)],
            )
            .await?;
        conn.commit().await?;
        Ok((deleted >= 1).then_some(version))
    }

    async fn version_is_soft_deleted(
        &self,
        subject: &str,
        version: i32,
    ) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                        WHERE sub.name = :1 AND sv.version = {version} AND sv.deleted = 1\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &[s(subject)],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
    }

    async fn version_is_active(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                        WHERE sub.name = :1 AND sv.version = {version} AND sv.deleted = 0\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &[s(subject)],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
    }

    // -- Compatibility config --

    async fn get_subject_level(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT compatibility_level FROM config \
                 WHERE subject = :1 AND compatibility_level IS NOT NULL",
                &[s(subject)],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| r.get_string(0))
            .map(str::to_owned))
    }

    async fn get_global_level(&self) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT COALESCE(compatibility_level, 'BACKWARD') FROM config WHERE subject IS NULL",
                &[],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| r.get_string(0))
            .unwrap_or("BACKWARD")
            .to_owned())
    }

    async fn set_global_level(&self, level: &str, normalize: bool) -> Result<String, KoraError> {
        let n = i64::from(normalize);
        let conn = self.conn().await?;
        conn.execute_dml_sql(
            &format!(
                "UPDATE config SET compatibility_level = :1, normalize = {n}, \
                 updated_at = SYSTIMESTAMP WHERE subject IS NULL"
            ),
            &[s(level)],
        )
        .await?;
        conn.commit().await?;
        Ok(level.to_owned())
    }

    async fn reconcile_global_level(&self, level: &str) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        conn.execute_dml_sql(
            "UPDATE config SET compatibility_level = :1, updated_at = SYSTIMESTAMP \
             WHERE subject IS NULL",
            &[s(level)],
        )
        .await?;
        conn.commit().await?;
        Ok(level.to_owned())
    }

    async fn set_subject_level(
        &self,
        subject: &str,
        level: &str,
        normalize: bool,
    ) -> Result<String, KoraError> {
        let n = i64::from(normalize);
        let conn = self.conn().await?;
        let updated = conn
            .execute_dml_sql(
                &format!(
                    "UPDATE config SET compatibility_level = :1, normalize = {n}, \
                     updated_at = SYSTIMESTAMP WHERE subject = :2"
                ),
                &[s(level), s(subject)],
            )
            .await?;
        if updated == 0 {
            let insert = conn
                .execute_dml_sql(
                    &format!(
                        "INSERT INTO config (subject, compatibility_level, normalize) \
                         VALUES (:1, :2, {n})"
                    ),
                    &[s(subject), s(level)],
                )
                .await;
            match insert {
                Ok(_) => {}
                Err(e) if is_unique_violation(&e) => {
                    conn.execute_dml_sql(
                        &format!(
                            "UPDATE config SET compatibility_level = :1, normalize = {n}, \
                             updated_at = SYSTIMESTAMP WHERE subject = :2"
                        ),
                        &[s(level), s(subject)],
                    )
                    .await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.commit().await?;
        Ok(level.to_owned())
    }

    async fn delete_subject_level(
        &self,
        subject: &str,
    ) -> Result<Option<(String, bool)>, KoraError> {
        let conn = self.conn().await?;
        let current = conn
            .query(
                "SELECT compatibility_level, COALESCE(normalize, 0) FROM config \
                 WHERE subject = :1 AND compatibility_level IS NOT NULL FOR UPDATE",
                &[s(subject)],
            )
            .await?;
        let result = current.first().map(|row| {
            let level = row.get_string(0).unwrap_or_default().to_owned();
            let norm = val_i64(row.get(1)).unwrap_or(0) != 0;
            (level, norm)
        });
        if result.is_some() {
            conn.execute_dml_sql(
                "UPDATE config SET compatibility_level = NULL, normalize = NULL, \
                 updated_at = SYSTIMESTAMP WHERE subject = :1",
                &[s(subject)],
            )
            .await?;
            conn.execute_dml_sql(
                "DELETE FROM config \
                 WHERE subject = :1 AND compatibility_level IS NULL AND registry_mode IS NULL",
                &[s(subject)],
            )
            .await?;
        }
        conn.commit().await?;
        Ok(result)
    }

    async fn get_global_normalize(&self) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT COALESCE(normalize, 0) FROM config WHERE subject IS NULL",
                &[],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))).unwrap_or(0) != 0)
    }

    async fn get_subject_normalize(&self, subject: &str) -> Result<Option<bool>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT COALESCE(normalize, 0) FROM config \
                 WHERE subject = :1 AND compatibility_level IS NOT NULL",
                &[s(subject)],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| val_i64(r.get(0)))
            .map(|n| n != 0))
    }

    async fn delete_global_level(&self) -> Result<(String, bool), KoraError> {
        let conn = self.conn().await?;
        let current = conn
            .query(
                "SELECT COALESCE(compatibility_level, 'BACKWARD'), COALESCE(normalize, 0) \
                 FROM config WHERE subject IS NULL FOR UPDATE",
                &[],
            )
            .await?;
        let (level, normalize) = current.first().map_or_else(
            || ("BACKWARD".to_owned(), false),
            |row| {
                (
                    row.get_string(0).unwrap_or("BACKWARD").to_owned(),
                    val_i64(row.get(1)).unwrap_or(0) != 0,
                )
            },
        );
        conn.execute_dml_sql(
            "UPDATE config SET compatibility_level = 'BACKWARD', normalize = 0, \
             updated_at = SYSTIMESTAMP WHERE subject IS NULL",
            &[],
        )
        .await?;
        conn.commit().await?;
        Ok((level, normalize))
    }

    // -- Mode --

    async fn get_global_mode(&self) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL",
                &[],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| r.get_string(0))
            .unwrap_or("READWRITE")
            .to_owned())
    }

    async fn set_global_mode(&self, mode: &str) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        conn.execute_dml_sql(
            "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject IS NULL",
            &[s(mode)],
        )
        .await?;
        conn.commit().await?;
        Ok(mode.to_owned())
    }

    async fn delete_global_mode(&self) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        let current = conn
            .query(
                "SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL FOR UPDATE",
                &[],
            )
            .await?;
        let prev = current
            .first()
            .and_then(|r| r.get_string(0))
            .unwrap_or("READWRITE")
            .to_owned();
        conn.execute_dml_sql(
            "UPDATE config SET registry_mode = 'READWRITE', updated_at = SYSTIMESTAMP \
             WHERE subject IS NULL",
            &[],
        )
        .await?;
        conn.commit().await?;
        Ok(prev)
    }

    async fn get_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL",
                &[s(subject)],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| r.get_string(0))
            .map(str::to_owned))
    }

    async fn set_subject_mode(&self, subject: &str, mode: &str) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        let updated = conn
            .execute_dml_sql(
                "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP WHERE subject = :2",
                &[s(mode), s(subject)],
            )
            .await?;
        if updated == 0 {
            let insert = conn
                .execute_dml_sql(
                    "INSERT INTO config (subject, registry_mode) VALUES (:1, :2)",
                    &[s(subject), s(mode)],
                )
                .await;
            match insert {
                Ok(_) => {}
                Err(e) if is_unique_violation(&e) => {
                    conn.execute_dml_sql(
                        "UPDATE config SET registry_mode = :1, updated_at = SYSTIMESTAMP \
                         WHERE subject = :2",
                        &[s(mode), s(subject)],
                    )
                    .await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.commit().await?;
        Ok(mode.to_owned())
    }

    async fn delete_subject_mode(&self, subject: &str) -> Result<Option<String>, KoraError> {
        let conn = self.conn().await?;
        let current = conn
            .query(
                "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
                &[s(subject)],
            )
            .await?;
        let prev = current
            .first()
            .and_then(|r| r.get_string(0))
            .map(str::to_owned);
        if prev.is_some() {
            conn.execute_dml_sql(
                "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
                &[s(subject)],
            )
            .await?;
            conn.execute_dml_sql(
                "DELETE FROM config \
                 WHERE subject = :1 AND compatibility_level IS NULL AND registry_mode IS NULL",
                &[s(subject)],
            )
            .await?;
        }
        conn.commit().await?;
        Ok(prev)
    }

    async fn delete_subject_mode_recursive(
        &self,
        subject: &str,
    ) -> Result<Option<String>, KoraError> {
        let conn = self.conn().await?;
        let current = conn
            .query(
                "SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL FOR UPDATE",
                &[s(subject)],
            )
            .await?;
        let prev = current
            .first()
            .and_then(|r| r.get_string(0))
            .map(str::to_owned);
        if prev.is_some() {
            conn.execute_dml_sql(
                "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP WHERE subject = :1",
                &[s(subject)],
            )
            .await?;
        }
        // Children: starts-with via INSTR (no LIKE-wildcard injection).
        conn.execute_dml_sql(
            "UPDATE config SET registry_mode = NULL, updated_at = SYSTIMESTAMP \
             WHERE INSTR(subject, :1) = 1 AND subject != :2 AND registry_mode IS NOT NULL",
            &[s(subject), s(subject)],
        )
        .await?;
        conn.execute_dml_sql(
            "DELETE FROM config \
             WHERE (subject = :1 OR (INSTR(subject, :2) = 1 AND subject != :3)) \
               AND compatibility_level IS NULL AND registry_mode IS NULL",
            &[s(subject), s(subject), s(subject)],
        )
        .await?;
        conn.commit().await?;
        Ok(prev)
    }

    async fn get_effective_mode(&self, subject: &str) -> Result<String, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT COALESCE(\
                    (SELECT registry_mode FROM config WHERE subject = :1 AND registry_mode IS NOT NULL), \
                    (SELECT COALESCE(registry_mode, 'READWRITE') FROM config WHERE subject IS NULL)\
                 ) FROM dual",
                &[s(subject)],
            )
            .await?;
        Ok(result
            .first()
            .and_then(|r| r.get_string(0))
            .unwrap_or("READWRITE")
            .to_owned())
    }

    // -- References --

    async fn validate_references(&self, refs: &[SchemaReference]) -> Result<(), KoraError> {
        let conn = self.conn().await?;
        for r in refs {
            let result = conn
                .query(
                    &format!(
                        "SELECT CASE WHEN EXISTS (\
                            SELECT 1 FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id \
                            WHERE sub.name = :1 AND sv.version = {} \
                              AND sv.deleted = 0 AND sub.deleted = 0\
                         ) THEN 1 ELSE 0 END FROM dual",
                        r.version
                    ),
                    &[s(&r.subject)],
                )
                .await?;
            if result.first().and_then(|row| val_i64(row.get(0))) != Some(1) {
                return Err(KoraError::ReferenceNotFound(format!(
                    "Schema reference not found: subject '{}' version {}",
                    r.subject, r.version
                )));
            }
        }
        Ok(())
    }

    async fn find_references_by_schema_id(
        &self,
        content_id: i64,
    ) -> Result<Vec<SchemaReference>, KoraError> {
        let conn = self.conn().await?;
        // Bind the id: this is called once per row of a listing, so inlining would
        // create a distinct statement per call and exhaust the session's cursors.
        let result = conn
            .query(
                "SELECT name, subject, version FROM schema_references \
                 WHERE content_id = :1 ORDER BY name",
                &[Value::from(content_id)],
            )
            .await?;
        let mut out = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            out.push(SchemaReference {
                name: row.get_string(0).unwrap_or_default().to_owned(),
                subject: row.get_string(1).unwrap_or_default().to_owned(),
                version: cell_i32(row, 2)?,
            });
        }
        Ok(out)
    }

    async fn find_references_for_schema_ids(
        &self,
        content_ids: &[i64],
    ) -> Result<Vec<(i64, SchemaReference)>, KoraError> {
        if content_ids.is_empty() {
            return Ok(Vec::new());
        }
        // The content ids are internal i64s — inlined into one IN-list query
        // (a single statement per listing, vs an N+1 of per-id queries).
        let in_list = content_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT content_id, name, subject, version FROM schema_references \
                     WHERE content_id IN ({in_list}) ORDER BY content_id, name"
                ),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            out.push((
                cell_i64(row, 0)?,
                SchemaReference {
                    name: row.get_string(1).unwrap_or_default().to_owned(),
                    subject: row.get_string(2).unwrap_or_default().to_owned(),
                    version: cell_i32(row, 3)?,
                },
            ));
        }
        Ok(out)
    }

    async fn find_referencing_schema_ids(
        &self,
        target_subject: &str,
        target_version: i32,
        include_deleted: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<i64>, KoraError> {
        let incl = i64::from(include_deleted);
        let sql = format!(
            "SELECT DISTINCT sr.content_id FROM schema_references sr \
             JOIN schema_versions sv ON sr.content_id = sv.content_id \
             WHERE sr.subject = :1 AND sr.version = {target_version} \
               AND (sv.deleted = 0 OR {incl} = 1) \
             ORDER BY sr.content_id{}",
            page_clause(offset, limit)
        );
        let conn = self.conn().await?;
        let result = conn.query(&sql, &[s(target_subject)]).await?;
        let mut out = Vec::with_capacity(result.row_count());
        for row in result.iter() {
            out.push(cell_i64(row, 0)?);
        }
        Ok(out)
    }

    async fn is_version_referenced(&self, subject: &str, version: i32) -> Result<bool, KoraError> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                &format!(
                    "SELECT CASE WHEN EXISTS (\
                        SELECT 1 FROM schema_references sr \
                        JOIN schema_versions sv ON sr.content_id = sv.content_id \
                        WHERE sr.subject = :1 AND sr.version = {version} AND sv.deleted = 0\
                     ) THEN 1 ELSE 0 END FROM dual"
                ),
                &[s(subject)],
            )
            .await?;
        Ok(result.first().and_then(|r| val_i64(r.get(0))) == Some(1))
    }
}

// -- Registration helpers --

/// Upsert a subject by name and return its id, holding a row lock on it.
async fn upsert_subject(conn: &Connection, name: &str) -> Result<i64, KoraError> {
    let updated = conn
        .execute_dml_sql(
            "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
            &[s(name)],
        )
        .await?;
    if updated == 0 {
        // Insert; a concurrent insert (ORA-00001) means the row now exists, so
        // re-run the update to re-activate and lock it.
        match conn
            .execute_dml_sql("INSERT INTO subjects (name) VALUES (:1)", &[s(name)])
            .await
        {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                conn.execute_dml_sql(
                    "UPDATE subjects SET deleted = 0, updated_at = SYSTIMESTAMP WHERE name = :1",
                    &[s(name)],
                )
                .await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let result = conn
        .query("SELECT id FROM subjects WHERE name = :1", &[s(name)])
        .await?;
    result
        .first()
        .and_then(|row| val_i64(row.get(0)))
        .ok_or_else(|| KoraError::BackendDataStore("subject id missing after upsert".to_owned()))
}

/// Deduplicate schema content globally and return its id (`ON CONFLICT
/// (raw_fingerprint)` equivalent).
async fn upsert_content(conn: &Connection, schema: &NewSchema<'_>) -> Result<i64, KoraError> {
    let raw_fp = schema.raw_fingerprint;
    let existing = conn
        .query(
            "SELECT id FROM schema_contents WHERE raw_fingerprint = :1",
            &[s(raw_fp)],
        )
        .await?;
    if let Some(id) = existing.first().and_then(|row| val_i64(row.get(0))) {
        // Mirror EXCLUDED.schema_type from the Postgres upsert.
        conn.execute_dml_sql(
            &format!("UPDATE schema_contents SET schema_type = :1 WHERE id = {id}"),
            &[s(schema.schema_type)],
        )
        .await?;
        return Ok(id);
    }

    let insert = conn
        .execute_dml_sql(
            "INSERT INTO schema_contents \
                (schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint) \
             VALUES (:1, :2, :3, :4, :5)",
            &[
                s(schema.schema_type),
                s(schema.schema_text),
                s(schema.canonical_form),
                s(schema.fingerprint),
                s(raw_fp),
            ],
        )
        .await;
    match insert {
        Ok(_) => {}
        Err(e) if is_unique_violation(&e) => {} // concurrent insert — re-select below
        Err(e) => return Err(e.into()),
    }
    let result = conn
        .query(
            "SELECT id FROM schema_contents WHERE raw_fingerprint = :1",
            &[s(raw_fp)],
        )
        .await?;
    result
        .first()
        .and_then(|row| val_i64(row.get(0)))
        .ok_or_else(|| KoraError::BackendDataStore("content id missing after upsert".to_owned()))
}

/// Run the in-transaction compatibility check (mirror of the Postgres path).
async fn run_compat_check(
    conn: &Connection,
    subject_id: i64,
    compat: CompatCheck,
) -> Result<(), KoraError> {
    let versions = if compat.versions.is_empty() {
        let result = conn
            .query(
                &format!(
                    "SELECT {SV_COLS_META} {SV_JOIN} \
                     WHERE sv.subject_id = {subject_id} AND sv.deleted = 0 ORDER BY sv.version"
                ),
                &[],
            )
            .await?;
        collect_svs(conn, &result).await?
    } else {
        compat.versions
    };

    for existing in &versions {
        let Ok(existing_format) = SchemaFormat::from_optional(Some(&existing.schema_type)) else {
            continue;
        };
        if existing_format != compat.format {
            continue;
        }
        let result = schema::check_compatibility(
            compat.format,
            &compat.new_schema,
            &existing.schema,
            compat.direction,
        )?;
        if !result.is_compatible {
            return Err(KoraError::IncompatibleSchema);
        }
    }
    Ok(())
}
