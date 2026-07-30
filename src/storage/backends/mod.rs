//! Storage backend adapters.
//!
//! `PostgreSQL` is the only backing store: [`PgStorage`] is a self-contained
//! adapter implementing the [`crate::storage::Storage`] trait directly over
//! `sqlx`.

pub mod postgres;
pub use postgres::PgStorage;
