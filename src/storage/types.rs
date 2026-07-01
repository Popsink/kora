//! Backend-agnostic domain types shared by every storage backend.
//!
//! These describe *what* the storage layer exchanges with the rest of the app
//! (schemas, versions, compatibility inputs, delete outcomes) independently of
//! *how* any backend persists them. Each backend adapter and the `Storage` trait
//! depend on these; no SQL or driver detail lives here.

/// Data needed to insert a new schema version.
pub struct NewSchema<'a> {
    /// Format identifier (e.g. "AVRO").
    pub schema_type: &'a str,
    /// Original schema text as submitted by the client.
    pub schema_text: &'a str,
    /// Canonical form used for deduplication.
    pub canonical_form: &'a str,
    /// Fingerprint of the canonical form (for normalized dedup).
    pub fingerprint: &'a str,
    /// Fingerprint of the raw schema text (for non-normalized dedup).
    pub raw_fingerprint: &'a str,
}

/// A subject-version pair, returned by schema ID cross-reference lookups.
#[derive(Debug, serde::Serialize)]
pub struct SubjectVersion {
    /// Subject name.
    pub subject: String,
    /// Version number within the subject.
    pub version: i32,
}

/// A schema with its subject context, returned by version lookups.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaVersion {
    /// Subject name.
    pub subject: String,
    /// Global schema ID (content ID, shared across subjects with identical content).
    pub id: i64,
    /// Version number within the subject.
    pub version: i32,
    /// Raw schema text.
    pub schema: String,
    /// Schema format (always included — Confluent serializes "AVRO" via `NON_EMPTY`).
    #[serde(rename = "schemaType")]
    pub schema_type: String,
    /// Schema references (Protobuf imports, JSON Schema `$ref`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<crate::types::SchemaReference>,
}

/// A compatibility check to run inside the registration transaction.
///
/// Populated by the handler with the schemas to check against. Running inside the
/// transaction (after locking the subject row) guarantees no other registration
/// can slip between the check and the insert.
#[derive(Clone)]
pub struct CompatCheck {
    /// Schemas to check compatibility against (fetched before the transaction for
    /// the non-transitive case, or inside it for transitive).
    pub versions: Vec<SchemaVersion>,
    /// The new schema text to check.
    pub new_schema: String,
    /// The format of the new schema.
    pub format: crate::schema::SchemaFormat,
    /// The compatibility direction resolved from the configured level.
    pub direction: crate::schema::CompatDirection,
}

/// Outcome of a hard-delete-subject request.
#[derive(Debug, PartialEq, Eq)]
pub enum HardDeleteResult {
    /// Subject removed; carries its (sorted) version numbers.
    Deleted(Vec<i32>),
    /// Subject exists but is still active (must be soft-deleted first).
    NotSoftDeleted,
    /// Subject does not exist.
    NotFound,
    /// A still-active version references this subject, blocking deletion.
    ReferenceExists(String),
}
