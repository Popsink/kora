//! Unit tests for schema parsing, canonical form, and fingerprinting.

mod common;

use kora::schema::{self, SchemaFormat};

// -- SchemaFormat dispatch --

#[test]
fn format_default_is_avro() {
    assert_eq!(
        SchemaFormat::from_optional(None).unwrap(),
        SchemaFormat::Avro
    );
}

#[test]
fn format_avro_accepted() {
    assert_eq!(
        SchemaFormat::from_optional(Some("AVRO")).unwrap(),
        SchemaFormat::Avro
    );
    assert_eq!(
        SchemaFormat::from_optional(Some("avro")).unwrap(),
        SchemaFormat::Avro
    );
    assert_eq!(
        SchemaFormat::from_optional(Some("Avro")).unwrap(),
        SchemaFormat::Avro
    );
}

#[test]
fn format_json_accepted() {
    assert_eq!(
        SchemaFormat::from_optional(Some("JSON")).unwrap(),
        SchemaFormat::Json
    );
    assert_eq!(
        SchemaFormat::from_optional(Some("json")).unwrap(),
        SchemaFormat::Json
    );
}

#[test]
fn format_protobuf_accepted() {
    assert_eq!(
        SchemaFormat::from_optional(Some("PROTOBUF")).unwrap(),
        SchemaFormat::Protobuf
    );
    assert_eq!(
        SchemaFormat::from_optional(Some("protobuf")).unwrap(),
        SchemaFormat::Protobuf
    );
}

#[test]
fn format_unsupported_errors() {
    let err = SchemaFormat::from_optional(Some("XML")).unwrap_err();
    assert!(err.to_string().contains("Unsupported schema type"));
}

// -- Avro Schema parsing --

#[test]
fn avro_parse_valid() {
    let result = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1);
    assert!(result.is_ok());
}

#[test]
fn avro_parse_invalid() {
    let result = schema::parse(SchemaFormat::Avro, r#"{"not": "a schema"}"#);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid schema"));
}

#[test]
fn avro_canonical_form_is_stable() {
    let a = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    assert_eq!(a.canonical_form, b.canonical_form);
    assert!(!a.canonical_form.is_empty());
}

#[test]
fn avro_fingerprint_is_stable() {
    let a = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
    assert!(!a.fingerprint.is_empty());
}

#[test]
fn avro_different_schemas_have_different_fingerprints() {
    let a = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    let b = schema::parse(
        SchemaFormat::Avro,
        r#"{"type":"record","name":"Other","fields":[{"name":"x","type":"string"}]}"#,
    )
    .unwrap();
    assert_ne!(a.fingerprint, b.fingerprint);
}

#[test]
fn avro_parse_decimal_with_bytes_default() {
    // Regression: apache-avro 0.21 rejected a JSON-string default on a `bytes` decimal (#86).
    let result = schema::parse(SchemaFormat::Avro, common::AVRO_SCHEMA_DECIMAL_DEFAULT);
    assert!(result.is_ok(), "{:?}", result.err());
}

// -- JSON Schema parsing --

#[test]
fn json_parse_valid() {
    let result = schema::parse(SchemaFormat::Json, common::JSON_SCHEMA_V1);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.canonical_form.is_empty());
    assert!(!parsed.fingerprint.is_empty());
}

#[test]
fn json_parse_invalid() {
    let result = schema::parse(SchemaFormat::Json, "not json at all");
    assert!(result.is_err());
}

#[test]
fn json_parse_valid_json_but_invalid_schema() {
    let result = schema::parse(SchemaFormat::Json, r#"{"type":"unicorn"}"#);
    assert!(result.is_err());
}

#[test]
fn json_parse_non_object_rejected() {
    assert!(schema::parse(SchemaFormat::Json, "true").is_err());
    assert!(schema::parse(SchemaFormat::Json, "null").is_err());
    assert!(schema::parse(SchemaFormat::Json, "[]").is_err());
}

#[test]
fn json_fingerprint_is_stable() {
    let a = schema::parse(SchemaFormat::Json, common::JSON_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Json, common::JSON_SCHEMA_V1).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn json_different_schemas_have_different_fingerprints() {
    let a = schema::parse(SchemaFormat::Json, common::JSON_SCHEMA_V1).unwrap();
    let b = schema::parse(
        SchemaFormat::Json,
        r#"{"type":"array","items":{"type":"integer"}}"#,
    )
    .unwrap();
    assert_ne!(a.fingerprint, b.fingerprint);
}

#[test]
fn json_canonical_form_sorts_keys() {
    let a = schema::parse(
        SchemaFormat::Json,
        r#"{"properties":{"name":{"type":"string"}},"type":"object"}"#,
    )
    .unwrap();
    let b = schema::parse(
        SchemaFormat::Json,
        r#"{"type":"object","properties":{"name":{"type":"string"}}}"#,
    )
    .unwrap();
    assert_eq!(a.canonical_form, b.canonical_form);
    assert_eq!(a.fingerprint, b.fingerprint);
}

// -- Protobuf parsing --

#[test]
fn protobuf_parse_valid() {
    let result = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1);
    assert!(result.is_ok());
    let parsed = result.unwrap();
    assert!(!parsed.canonical_form.is_empty());
    assert!(!parsed.fingerprint.is_empty());
}

#[test]
fn protobuf_parse_invalid() {
    let result = schema::parse(SchemaFormat::Protobuf, "not a proto file {{{");
    assert!(result.is_err());
}

#[test]
fn protobuf_canonical_form_is_stable() {
    let a = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1).unwrap();
    assert_eq!(a.canonical_form, b.canonical_form);
    assert!(!a.canonical_form.is_empty());
}

#[test]
fn protobuf_fingerprint_is_stable() {
    let a = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn protobuf_different_schemas_have_different_fingerprints() {
    let a = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V1).unwrap();
    let b = schema::parse(SchemaFormat::Protobuf, common::PROTO_SCHEMA_V2).unwrap();
    assert_ne!(a.fingerprint, b.fingerprint);
}
