//! Backend-neutral bound parameter.
//!
//! Simple methods pass values as `Bind`s and let each backend's [`SqlExecutor`]
//! lower them to its driver's native parameter type. Only the scalar types that
//! cross the query boundary in shared (non-transactional) methods are needed:
//! subject/schema names, fingerprints, levels, modes (strings), ids/versions
//! (integers), and the `include_deleted` flags (booleans).
//!
//! [`SqlExecutor`]: super::SqlExecutor

/// A value bound into a parameterized query.
#[derive(Debug, Clone)]
pub enum Bind {
    /// A text value.
    Str(String),
    /// A signed integer (ids, versions — `i32` is widened to `i64`).
    I64(i64),
    /// A boolean (`include_deleted` and similar flags).
    Bool(bool),
}

impl From<&str> for Bind {
    fn from(v: &str) -> Self {
        Bind::Str(v.to_owned())
    }
}

impl From<&String> for Bind {
    fn from(v: &String) -> Self {
        Bind::Str(v.clone())
    }
}

impl From<String> for Bind {
    fn from(v: String) -> Self {
        Bind::Str(v)
    }
}

impl From<i64> for Bind {
    fn from(v: i64) -> Self {
        Bind::I64(v)
    }
}

impl From<i32> for Bind {
    fn from(v: i32) -> Self {
        Bind::I64(i64::from(v))
    }
}

impl From<bool> for Bind {
    fn from(v: bool) -> Self {
        Bind::Bool(v)
    }
}

/// Build a fixed-size `[Bind; N]` from heterogeneous values:
/// `binds![name, version, flag]`. Used as `&binds![..]`, the array coerces to the
/// `&[Bind]` the executor and helpers expect — no heap allocation, and no
/// `clippy::useless_vec` at the call sites.
#[macro_export]
macro_rules! binds {
    ($($v:expr),* $(,)?) => {
        [$($crate::storage::sql::Bind::from($v)),*]
    };
}
