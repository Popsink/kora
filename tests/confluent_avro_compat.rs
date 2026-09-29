//! Avro compatibility tests replayed from Confluent Schema Registry.
//!
//! Ensures that `apache-avro`-backed compatibility checks match Confluent's
//! `AvroCompatibilityTest.java` behavior across all modes: BACKWARD, FORWARD,
//! FULL, and their TRANSITIVE variants.
//!
//! Source: `core/src/test/java/io/confluent/kafka/schemaregistry/avro/AvroCompatibilityTest.java`

use kora::schema::CompatDirection;
use kora::schema::avro::check_compatibility;

// ---------------------------------------------------------------------------
// Schema definitions — exact replicas from the Confluent Java test.
// ---------------------------------------------------------------------------

const SCHEMA1: &str =
    r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"}]}"#;

const SCHEMA2: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2","default":"foo"}]}"#;

const SCHEMA3: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2"}]}"#;

const SCHEMA4: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1_new","aliases":["f1"]}]}"#;

const SCHEMA6: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":["null","string"],"name":"f1","doc":"doc of f1"}]}"#;

const SCHEMA7: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":["null","string","int"],"name":"f1","doc":"doc of f1"}]}"#;

const SCHEMA8: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2","default":"foo"},{"type":"string","name":"f3","default":"bar"}]}"#;

const BAD_DEFAULT_NULL: &str = r#"{"type":"record","name":"myrecord","fields":[{"type":["null","string"],"name":"f1","default":"null"},{"type":"string","name":"f2","default":"foo"},{"type":"string","name":"f3","default":"bar"}]}"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check a new schema against a single existing schema.
fn is_compatible(new: &str, existing: &str, direction: CompatDirection) -> bool {
    check_compatibility(new, existing, direction)
        .expect("both schemas should parse")
        .is_compatible
}

/// Transitive check: new schema must be compatible with ALL existing schemas.
fn is_compatible_transitive(new: &str, existing: &[&str], direction: CompatDirection) -> bool {
    existing
        .iter()
        .all(|old| is_compatible(new, old, direction))
}

// ---------------------------------------------------------------------------
// testBadDefaultNull
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_bad_default_null() {
    // Confluent: assertNotNull(AvroUtils.parseSchema(badDefaultNullString))
    let result = kora::schema::avro::parse(BAD_DEFAULT_NULL);
    assert!(
        result.is_ok(),
        "schema with \"null\" default string should parse: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// testBasicBackwardsCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_backward_add_field_with_default() {
    // "adding a field with default is a backward compatible change"
    assert!(is_compatible(SCHEMA2, SCHEMA1, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_add_field_without_default() {
    // "adding a field w/o default is not a backward compatible change"
    assert!(!is_compatible(SCHEMA3, SCHEMA1, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_rename_field_with_alias() {
    // "changing field name with alias is a backward compatible change"
    assert!(is_compatible(SCHEMA4, SCHEMA1, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_evolve_to_union() {
    // "evolving a field type to a union is a backward compatible change"
    assert!(is_compatible(SCHEMA6, SCHEMA1, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_remove_type_from_union() {
    // "removing a type from a union is not a backward compatible change"
    assert!(!is_compatible(SCHEMA1, SCHEMA6, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_add_type_to_union() {
    // "adding a new type in union is a backward compatible change"
    assert!(is_compatible(SCHEMA7, SCHEMA6, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_shrink_union() {
    // "removing a type from a union is not a backward compatible change"
    assert!(!is_compatible(SCHEMA6, SCHEMA7, CompatDirection::Backward));
}

#[test]
fn confluent_avro_backward_non_transitive_remove_default() {
    // Non-transitive backward: schema3 vs last-of [schema1, schema2] = schema2.
    // "removing a default is not a transitively compatible change"
    // (but non-transitive only checks the latest, so it passes)
    assert!(is_compatible(SCHEMA3, SCHEMA2, CompatDirection::Backward));
}

// ---------------------------------------------------------------------------
// testBasicBackwardsTransitiveCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_backward_transitive_iterative_defaults() {
    // "iteratively adding fields with defaults is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA8,
        &[SCHEMA1, SCHEMA2],
        CompatDirection::Backward,
    ));
}

#[test]
fn confluent_avro_backward_transitive_add_default() {
    // "adding a field with default is a backward compatible change"
    assert!(is_compatible_transitive(
        SCHEMA2,
        &[SCHEMA1],
        CompatDirection::Backward,
    ));
}

#[test]
fn confluent_avro_backward_transitive_remove_default_single() {
    // "removing a default is a compatible change, but not transitively"
    assert!(is_compatible_transitive(
        SCHEMA3,
        &[SCHEMA2],
        CompatDirection::Backward,
    ));
}

#[test]
fn confluent_avro_backward_transitive_remove_default_fails() {
    // "removing a default is not a transitively compatible change"
    assert!(!is_compatible_transitive(
        SCHEMA3,
        &[SCHEMA2, SCHEMA1],
        CompatDirection::Backward,
    ));
}

// ---------------------------------------------------------------------------
// testBasicForwardsCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_forward_add_field_with_default() {
    // "adding a field is a forward compatible change"
    assert!(is_compatible(SCHEMA2, SCHEMA1, CompatDirection::Forward));
}

#[test]
fn confluent_avro_forward_add_field_without_default() {
    // "adding a field is a forward compatible change"
    assert!(is_compatible(SCHEMA3, SCHEMA1, CompatDirection::Forward));
}

#[test]
fn confluent_avro_forward_add_field_to_schema_with_default() {
    // "adding a field is a forward compatible change"
    assert!(is_compatible(SCHEMA3, SCHEMA2, CompatDirection::Forward));
}

#[test]
fn confluent_avro_forward_add_default_to_field() {
    // "adding a field is a forward compatible change"
    assert!(is_compatible(SCHEMA2, SCHEMA3, CompatDirection::Forward));
}

#[test]
fn confluent_avro_forward_non_transitive_remove_default() {
    // Non-transitive forward: schema1 vs last-of [schema3, schema2] = schema2.
    // "removing a default is not a transitively compatible change"
    // (but non-transitive only checks the latest, so it passes)
    assert!(is_compatible(SCHEMA1, SCHEMA2, CompatDirection::Forward));
}

// ---------------------------------------------------------------------------
// testBasicForwardsTransitiveCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_forward_transitive_iterative_remove_defaults() {
    // "iteratively removing fields with defaults is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA8, SCHEMA2],
        CompatDirection::Forward,
    ));
}

#[test]
fn confluent_avro_forward_transitive_add_default() {
    // "adding default to a field is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA2,
        &[SCHEMA3],
        CompatDirection::Forward,
    ));
}

#[test]
fn confluent_avro_forward_transitive_remove_field_with_default() {
    // "removing a field with a default is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA2],
        CompatDirection::Forward,
    ));
}

#[test]
fn confluent_avro_forward_transitive_remove_default_fails() {
    // "removing a default is not a transitively compatible change"
    assert!(!is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA2, SCHEMA3],
        CompatDirection::Forward,
    ));
}

// ---------------------------------------------------------------------------
// testBasicFullCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_full_add_field_with_default() {
    // "adding a field with default is a backward and a forward compatible change"
    assert!(is_compatible(SCHEMA2, SCHEMA1, CompatDirection::Full));
}

#[test]
fn confluent_avro_full_non_transitive_add_without_default() {
    // Non-transitive full: schema3 vs last-of [schema1, schema2] = schema2.
    // "transitively adding a field without a default is not a compatible change"
    // (but non-transitive only checks the latest, so it passes)
    assert!(is_compatible(SCHEMA3, SCHEMA2, CompatDirection::Full));
}

#[test]
fn confluent_avro_full_non_transitive_remove_without_default() {
    // Non-transitive full: schema1 vs last-of [schema3, schema2] = schema2.
    // "transitively removing a field without a default is not a compatible change"
    // (but non-transitive only checks the latest, so it passes)
    assert!(is_compatible(SCHEMA1, SCHEMA2, CompatDirection::Full));
}

// ---------------------------------------------------------------------------
// testBasicFullTransitiveCompatibility
// ---------------------------------------------------------------------------

#[test]
fn confluent_avro_full_transitive_iterative_add_defaults() {
    // "iteratively adding fields with defaults is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA8,
        &[SCHEMA1, SCHEMA2],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_iterative_remove_defaults() {
    // "iteratively removing fields with defaults is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA8, SCHEMA2],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_add_default_to_field() {
    // "adding default to a field is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA2,
        &[SCHEMA3],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_remove_field_with_default() {
    // "removing a field with a default is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA2],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_add_field_with_default() {
    // "adding a field with default is a compatible change"
    assert!(is_compatible_transitive(
        SCHEMA2,
        &[SCHEMA1],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_remove_default() {
    // "removing a default from a field compatible change"
    assert!(is_compatible_transitive(
        SCHEMA3,
        &[SCHEMA2],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_add_without_default_fails() {
    // "transitively adding a field without a default is not a compatible change"
    assert!(!is_compatible_transitive(
        SCHEMA3,
        &[SCHEMA2, SCHEMA1],
        CompatDirection::Full,
    ));
}

#[test]
fn confluent_avro_full_transitive_remove_without_default_fails() {
    // "transitively removing a field without a default is not a compatible change"
    assert!(!is_compatible_transitive(
        SCHEMA1,
        &[SCHEMA2, SCHEMA3],
        CompatDirection::Full,
    ));
}

// ---------------------------------------------------------------------------
// Partial compatibility (not in the Confluent test; Avro Java semantics)
// ---------------------------------------------------------------------------

const ENUM_ABC: &str = r#"{"type":"record","name":"myrecord","fields":[{"name":"e","type":{"type":"enum","name":"E","symbols":["A","B","C"]}}]}"#;
const ENUM_AB: &str = r#"{"type":"record","name":"myrecord","fields":[{"name":"e","type":{"type":"enum","name":"E","symbols":["A","B"]}}]}"#;
const ENUM_AB_DEFAULT: &str = r#"{"type":"record","name":"myrecord","fields":[{"name":"e","type":{"type":"enum","name":"E","symbols":["A","B"],"default":"A"}}]}"#;

#[test]
fn avro_backward_remove_enum_symbol_fails() {
    // Old writer may emit "C", which the new reader cannot resolve.
    let result = check_compatibility(ENUM_AB, ENUM_ABC, CompatDirection::Backward).unwrap();
    assert!(!result.is_compatible);
    assert!(!result.messages.is_empty());
}

#[test]
fn avro_backward_remove_enum_symbol_with_reader_default() {
    assert!(is_compatible(
        ENUM_AB_DEFAULT,
        ENUM_ABC,
        CompatDirection::Backward
    ));
}
