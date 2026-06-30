//! Backend-agnostic compatibility evaluation.
//!
//! The *fetch* of the versions to check against is backend-specific (it runs
//! inside each backend's registration transaction); the *evaluation* of the new
//! schema against those versions is pure and identical everywhere, so it lives
//! here and is shared by every backend's `register` path.

use crate::error::KoraError;
use crate::schema::{self, SchemaFormat};
use crate::storage::types::{CompatCheck, SchemaVersion};

/// Check `compat.new_schema` against each already-materialized `version`.
///
/// Versions whose format differs from the new schema's are skipped (mixed-format
/// subjects). Returns [`KoraError::IncompatibleSchema`] on the first violation.
///
/// # Errors
///
/// Propagates parser/compatibility errors from [`schema::check_compatibility`],
/// and returns [`KoraError::IncompatibleSchema`] when a version is incompatible.
pub fn evaluate(versions: &[SchemaVersion], compat: &CompatCheck) -> Result<(), KoraError> {
    for existing in versions {
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
