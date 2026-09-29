//! Avro schema parsing, canonical form, fingerprinting, and compatibility.

use apache_avro::Schema;
use apache_avro::error::CompatibilityError;
use apache_avro::rabin::Rabin;
use apache_avro::schema_compatibility::{Compatibility, SchemaCompatibility};

use super::{CompatDirection, CompatibilityResult};
use crate::error::KoraError;

// -- Parsing --

/// Parse an Avro schema string and compute its canonical form and Rabin fingerprint.
///
/// # Errors
///
/// Returns `KoraError::InvalidSchema` when the input is not valid Avro JSON.
pub fn parse(raw: &str) -> Result<(String, String), KoraError> {
    let schema = Schema::parse_str(raw).map_err(|e| KoraError::InvalidSchema(e.to_string()))?;

    let canonical = schema.canonical_form();
    let fingerprint = schema.fingerprint::<Rabin>().to_string();

    Ok((canonical, fingerprint))
}

// -- Compatibility --

/// Check compatibility between two Avro schemas under a given mode.
///
/// - `BACKWARD`/`BACKWARD_TRANSITIVE`: new can read old (writer=old, reader=new).
/// - `FORWARD`/`FORWARD_TRANSITIVE`: old can read new (writer=new, reader=old).
/// - `FULL`/`FULL_TRANSITIVE`: both directions (`mutual_read`).
///
/// # Errors
///
/// Returns `KoraError::InvalidSchema` if either schema is malformed.
pub fn check_compatibility(
    new_schema: &str,
    existing_schema: &str,
    direction: CompatDirection,
) -> Result<CompatibilityResult, KoraError> {
    let new = Schema::parse_str(new_schema).map_err(|e| KoraError::InvalidSchema(e.to_string()))?;
    let existing =
        Schema::parse_str(existing_schema).map_err(|e| KoraError::InvalidSchema(e.to_string()))?;

    let result = match direction {
        // New reads old: writer=existing, reader=new.
        CompatDirection::Backward => SchemaCompatibility::can_read(&existing, &new),
        // Old reads new: writer=new, reader=existing.
        CompatDirection::Forward => SchemaCompatibility::can_read(&new, &existing),
        CompatDirection::Full => SchemaCompatibility::mutual_read(&existing, &new),
        CompatDirection::None => {
            return Ok(CompatibilityResult {
                is_compatible: true,
                messages: Vec::new(),
            });
        }
    };

    match result {
        Ok(Compatibility::Full) => Ok(CompatibilityResult {
            is_compatible: true,
            messages: Vec::new(),
        }),
        // Some writer values cannot be resolved (narrowed union, dropped enum symbol without
        // reader default). Confluent treats this as incompatible.
        Ok(Compatibility::Partial) => Ok(CompatibilityResult {
            is_compatible: false,
            messages: vec![
                "Reader schema cannot resolve every value of the writer schema (partial compatibility)"
                    .to_string(),
            ],
        }),
        Err(e) => Ok(CompatibilityResult {
            is_compatible: false,
            messages: collect_error_messages(&e),
        }),
    }
}

/// Recursively collect error messages from a `CompatibilityError` chain.
fn collect_error_messages(err: &CompatibilityError) -> Vec<String> {
    let mut msgs = vec![err.to_string()];
    if let Some(inner) =
        std::error::Error::source(err).and_then(|s| s.downcast_ref::<CompatibilityError>())
    {
        msgs.extend(collect_error_messages(inner));
    }
    msgs
}
