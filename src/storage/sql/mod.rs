//! Backend-neutral SQL toolkit.
//!
//! A small shared layer that lets every backend implement the bulk of the
//! `Storage` contract as one-line method bodies: [`Bind`] (neutral parameters),
//! [`Row`] (positional result decoding), [`SqlExecutor`] (the per-backend
//! execution surface), and [`helpers`] (generic execute-and-extract helpers).
//!
//! Dialect SQL strings stay in each backend's method bodies (greppable,
//! reviewable); only the boilerplate of running them and decoding rows is shared.

pub mod bind;
pub mod exec;
pub mod helpers;
pub mod row;

pub use bind::Bind;
pub use exec::SqlExecutor;
pub use row::Row;
