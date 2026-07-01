//! Backend-neutral row decoding.
//!
//! Each backend wraps its driver's row type and implements [`Row`] with
//! **positional** accessors (every backend reads result columns by index). This
//! is where dialect decode quirks are absorbed — e.g. Oracle returns `NUMBER`
//! as a decimal string and `NUMBER(1)` booleans as `0`/`1`, so its `get_i64` /
//! `get_bool` normalize those, leaving shared helpers backend-blind.

use crate::error::KoraError;

/// Positional column accessors over one result row, returning [`KoraError`] on a
/// type/range mismatch so callers stay backend-agnostic.
pub trait Row {
    /// Required `i64` column (ids, counts).
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if the column is absent or not an integer.
    fn get_i64(&self, idx: usize) -> Result<i64, KoraError>;

    /// Required `i32` column (version numbers).
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if absent, not an integer, or out of range.
    fn get_i32(&self, idx: usize) -> Result<i32, KoraError>;

    /// Required text column.
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if the column is absent or not text.
    fn get_str(&self, idx: usize) -> Result<String, KoraError>;

    /// Required boolean column (native `bool` or `NUMBER(1)` `0`/`1`).
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if the column is absent or not boolean-like.
    fn get_bool(&self, idx: usize) -> Result<bool, KoraError>;

    /// Nullable `i64` column (e.g. `MAX(id)` over an empty table).
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if the column is absent or not an integer.
    fn get_opt_i64(&self, idx: usize) -> Result<Option<i64>, KoraError>;

    /// Nullable text column.
    ///
    /// # Errors
    /// Returns [`KoraError::BackendDataStore`] if the column is absent or not text.
    fn get_opt_str(&self, idx: usize) -> Result<Option<String>, KoraError>;
}
