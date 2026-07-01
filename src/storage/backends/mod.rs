//! Storage backend adapters.
//!
//! Each backend is a self-contained adapter implementing the [`crate::storage::Storage`]
//! trait over the shared SQL toolkit (`crate::storage::sql`). Adding a new backend:
//! implement `SqlExecutor` + `Row` over its driver, write its dialect SQL in the
//! (mostly one-line) simple methods plus the handful of transactional methods, add a
//! migrations dir, and wire a `DbBackend` variant + `connect()` arm.

pub mod postgres;
pub use postgres::PgStorage;

#[cfg(feature = "oracle")]
pub mod oracle;
#[cfg(feature = "oracle")]
pub use oracle::OracleStorage;
