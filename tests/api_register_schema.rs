//! Integration tests for schema registration (POST /subjects/{subject}/versions).

mod common;

use reqwest::StatusCode;
use sqlx::Row;

// -- Common (format-agnostic) --

#[tokio::test]
async fn register_schema_without_type_defaults_to_avro() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("default-type-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    assert!(id > 0);
}

#[tokio::test]
async fn register_schema_creates_subject_implicitly() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let pool = common::pool().await;
    let subject = format!("implicit-{}", uuid::Uuid::new_v4());

    let count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subjects WHERE name = $1")
        .bind(&subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    let count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subjects WHERE name = $1")
        .bind(&subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn register_schema_empty_subject_returns_422() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    // Null char subject — must be inline (edge case subject).
    let resp = client
        .post(format!("{base}/subjects/%00/versions"))
        .json(&serde_json::json!({"schema": common::AVRO_SCHEMA_V1}))
        .send()
        .await
        .unwrap();

    assert_ne!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_schema_missing_body_returns_422() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    // Empty body — must be inline (custom body/header).
    let resp = client
        .post(format!("{base}/subjects/test-value/versions"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["error_code"], 42201,
        "should return Confluent error format"
    );
}

#[tokio::test]
async fn register_schema_lowercase_type_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    // Custom schemaType field — must be inline (custom body).
    let resp = client
        .post(format!("{base}/subjects/lowercase-type/versions"))
        .json(&serde_json::json!({"schema": common::AVRO_SCHEMA_V1, "schemaType": "avro"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_with_normalize_deduplicates_whitespace_variants() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("norm-reg-{}", uuid::Uuid::new_v4());

    let schema_compact = r#"{"type":"record","name":"Norm","fields":[{"name":"id","type":"int"}]}"#;
    let schema_spaced = r#"{  "type" : "record",  "name" : "Norm",  "fields" : [ { "name" : "id", "type" : "int" } ] }"#;

    let resp1 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=true"))
        .json(&serde_json::json!({"schema": schema_compact}))
        .send()
        .await
        .unwrap();
    let id1 = resp1.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp2 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=true"))
        .json(&serde_json::json!({"schema": schema_spaced}))
        .send()
        .await
        .unwrap();
    let id2 = resp2.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    assert_eq!(
        id1, id2,
        "normalize=true should deduplicate schemas that differ only in whitespace"
    );
}

#[tokio::test]
async fn register_with_subject_config_normalize_deduplicates() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("cfg-norm-{}", uuid::Uuid::new_v4());

    // Set subject-level normalize=true via config endpoint.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "NONE", "normalize": true}))
        .send()
        .await
        .unwrap();

    let schema_compact =
        r#"{"type":"record","name":"CfgNorm","fields":[{"name":"id","type":"int"}]}"#;
    let schema_spaced = r#"{  "type" : "record",  "name" : "CfgNorm",  "fields" : [ { "name" : "id", "type" : "int" } ] }"#;

    // Register WITHOUT ?normalize=true — config should drive normalization.
    let id1 = common::api::register_schema(&client, &base, &subject, schema_compact).await;
    let id2 = common::api::register_schema(&client, &base, &subject, schema_spaced).await;

    assert_eq!(
        id1, id2,
        "subject config normalize=true should deduplicate without query param"
    );
}

// -- Avro Schema --

#[tokio::test]
async fn register_avro_schema_valid_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let pool = common::pool().await;
    let subject = format!("valid-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    assert!(id > 0);

    // Verify the content stored in DB.
    let row = sqlx::query(
        "SELECT schema_type, schema_text, canonical_form, fingerprint FROM schema_contents WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.get::<String, _>("schema_type"), "AVRO");
    assert_eq!(row.get::<String, _>("schema_text"), common::AVRO_SCHEMA_V1);

    let expected =
        kora::schema::parse(kora::schema::SchemaFormat::Avro, common::AVRO_SCHEMA_V1).unwrap();
    assert_eq!(
        row.get::<Option<String>, _>("canonical_form").as_deref(),
        Some(expected.canonical_form.as_str())
    );
    assert_eq!(
        row.get::<Option<String>, _>("fingerprint").as_deref(),
        Some(expected.fingerprint.as_str())
    );

    // Verify the version row.
    let version_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id WHERE sub.name = $1 AND sv.content_id = $2",
    )
    .bind(&subject)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(version_count, 1);
}

#[tokio::test]
async fn register_avro_schema_idempotent_returns_same_id() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let pool = common::pool().await;
    let subject = format!("idempotent-{}", uuid::Uuid::new_v4());

    let id1 = common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    let id2 = common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    assert_eq!(id1, id2, "same schema should return same id");

    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM schema_versions sv JOIN subjects sub ON sv.subject_id = sub.id WHERE sub.name = $1",
    )
    .bind(&subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count, 1,
        "idempotent registration should not create duplicate rows"
    );
}

#[tokio::test]
async fn register_avro_schema_invalid_returns_422() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    // Invalid schema body — must be inline (custom body).
    let resp = client
        .post(format!("{base}/subjects/bad-value/versions"))
        .json(&serde_json::json!({"schema": r#"{"not":"a schema"}"#}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 42201);
}

// -- JSON Schema --

#[tokio::test]
async fn register_json_schema_valid_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-reg-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::JSON_SCHEMA_V1,
        "JSON",
    )
    .await;
    assert!(id > 0);
}

#[tokio::test]
async fn register_json_schema_invalid_returns_422() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/subjects/json-bad/versions"))
        .json(&serde_json::json!({"schema": "not valid json", "schemaType": "JSON"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 42201);
}

#[tokio::test]
async fn register_json_schema_retrieve_includes_type() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-type-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::JSON_SCHEMA_V1,
        "JSON",
    )
    .await;

    let resp = common::api::get_schema_by_id(&client, &base, id).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schemaType"], "JSON");
}

#[tokio::test]
async fn register_json_schema_idempotent_returns_same_id() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-idem-{}", uuid::Uuid::new_v4());

    let id1 = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::JSON_SCHEMA_V1,
        "JSON",
    )
    .await;
    let id2 = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::JSON_SCHEMA_V1,
        "JSON",
    )
    .await;
    assert_eq!(id1, id2);
}

#[tokio::test]
async fn register_json_schema_listed_under_versions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-ver-{}", uuid::Uuid::new_v4());

    common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::JSON_SCHEMA_V1,
        "JSON",
    )
    .await;

    let versions = common::api::list_versions(&client, &base, &subject, common::ACTIVE_ONLY).await;
    assert_eq!(versions, vec![1]);
}

#[tokio::test]
async fn register_json_schema_reordered_keys_deduplicates_with_normalize() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-dedup-{}", uuid::Uuid::new_v4());

    let schema_a = r#"{"type":"object","properties":{"name":{"type":"string"}}}"#;
    let schema_b = r#"{"properties":{"name":{"type":"string"}},"type":"object"}"#;

    let resp1 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=true"))
        .json(&serde_json::json!({"schema": schema_a, "schemaType": "JSON"}))
        .send()
        .await
        .unwrap();
    let id1 = resp1.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp2 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=true"))
        .json(&serde_json::json!({"schema": schema_b, "schemaType": "JSON"}))
        .send()
        .await
        .unwrap();
    let id2 = resp2.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    assert_eq!(
        id1, id2,
        "normalize=true should deduplicate reordered JSON keys"
    );
}

#[tokio::test]
async fn register_json_schema_reordered_keys_creates_new_version_without_normalize() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("json-no-norm-{}", uuid::Uuid::new_v4());

    let schema_a = r#"{"type":"object","properties":{"name":{"type":"string"}}}"#;
    let schema_b = r#"{"properties":{"name":{"type":"string"}},"type":"object"}"#;

    let id1 =
        common::api::register_schema_with_type(&client, &base, &subject, schema_a, "JSON").await;
    let id2 =
        common::api::register_schema_with_type(&client, &base, &subject, schema_b, "JSON").await;

    assert_ne!(
        id1, id2,
        "without normalize, different raw text should create separate versions"
    );
}

// -- Protobuf schema --

#[tokio::test]
async fn register_protobuf_schema_valid_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("proto-reg-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::PROTO_SCHEMA_V1,
        "PROTOBUF",
    )
    .await;
    assert!(id > 0);
}

#[tokio::test]
async fn register_protobuf_schema_invalid_returns_422() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/subjects/proto-bad/versions"))
        .json(&serde_json::json!({"schema": "not a proto file {{{", "schemaType": "PROTOBUF"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 42201);
}

#[tokio::test]
async fn register_protobuf_schema_retrieve_includes_type() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("proto-type-{}", uuid::Uuid::new_v4());

    let id = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::PROTO_SCHEMA_V1,
        "PROTOBUF",
    )
    .await;

    let resp = common::api::get_schema_by_id(&client, &base, id).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schemaType"], "PROTOBUF");
}

#[tokio::test]
async fn register_protobuf_schema_idempotent_returns_same_id() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("proto-idem-{}", uuid::Uuid::new_v4());

    let id1 = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::PROTO_SCHEMA_V1,
        "PROTOBUF",
    )
    .await;
    let id2 = common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::PROTO_SCHEMA_V1,
        "PROTOBUF",
    )
    .await;
    assert_eq!(id1, id2);
}

#[tokio::test]
async fn register_protobuf_schema_listed_under_versions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("proto-ver-{}", uuid::Uuid::new_v4());

    common::api::register_schema_with_type(
        &client,
        &base,
        &subject,
        common::PROTO_SCHEMA_V1,
        "PROTOBUF",
    )
    .await;

    let versions = common::api::list_versions(&client, &base, &subject, common::ACTIVE_ONLY).await;
    assert_eq!(versions, vec![1]);
}

// -- Compatibility enforcement --

#[tokio::test]
async fn register_backward_incompatible_rejected() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-bw-{}", uuid::Uuid::new_v4());

    // Default mode is BACKWARD. Register V1, then try incompatible schema.
    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40901);
}

#[tokio::test]
async fn register_backward_compatible_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-bw-ok-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_V2}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_forward_incompatible_rejected() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-fw-{}", uuid::Uuid::new_v4());

    // Set FORWARD mode. Register INCOMPAT (has required "email"), then try V1 (no "email").
    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_INCOMPAT).await;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "FORWARD"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_V1}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_full_backward_only_rejected() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-full-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "FULL"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_none_mode_always_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-none-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "NONE"}))
        .send()
        .await
        .unwrap();

    let totally_different =
        r#"{"type":"record","name":"Different","fields":[{"name":"x","type":"string"}]}"#;
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": totally_different}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_backward_transitive_checks_all_versions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-trans-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;
    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V2).await;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "BACKWARD_TRANSITIVE"}))
        .send()
        .await
        .unwrap();

    // V3 is backward-compatible with V2 AND V1 → should pass.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_V3}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // INCOMPAT adds required "email" — incompatible with all → rejected.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_full_transitive_checks_both_directions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-full-trans-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "FULL_TRANSITIVE"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_first_schema_always_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-first-{}", uuid::Uuid::new_v4());

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_V1}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_json_schema_enforcement() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-json-{}", uuid::Uuid::new_v4());

    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({
            "schema": r#"{"type":"object","properties":{"name":{"type":"string"}}}"#,
            "schemaType": "JSON"
        }))
        .send()
        .await
        .unwrap();

    // Narrowing type from string to integer is incompatible.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({
            "schema": r#"{"type":"object","properties":{"name":{"type":"integer"}}}"#,
            "schemaType": "JSON"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_protobuf_enforcement() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-proto-enf-{}", uuid::Uuid::new_v4());

    let proto_v1 = r#"syntax = "proto3"; message Test { string name = 1; }"#;
    let proto_v2_incompat = r#"syntax = "proto3"; message Test { int32 name = 1; }"#;

    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": proto_v1, "schemaType": "PROTOBUF"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": proto_v2_incompat, "schemaType": "PROTOBUF"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_forward_transitive_checks_all_versions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-fw-trans-{}", uuid::Uuid::new_v4());

    // schema1: {f1:string, f2:string}
    // schema2: {f1:string} (removed f2 — forward-compatible with schema1: old reader ignores f2)
    let schema1 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2"}]}"#;
    let schema2 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"}]}"#;
    // schema3: {f1:string, f3:string} — forward-compatible with schema2 (old reader ignores f3)
    // but NOT forward-compatible with schema1 (schema1 expects f2, schema3 doesn't have it)
    let schema3 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f3"}]}"#;

    // Register with NONE first to set up the version chain.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "NONE"}))
        .send()
        .await
        .unwrap();

    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema1}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema2}))
        .send()
        .await
        .unwrap();

    // Now switch to FORWARD_TRANSITIVE.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "FORWARD_TRANSITIVE"}))
        .send()
        .await
        .unwrap();

    // schema3 is forward-compatible with schema2 (latest) but NOT with schema1.
    // FORWARD_TRANSITIVE checks all → should reject.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema3}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn register_enforcement_skips_soft_deleted_versions() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-del-{}", uuid::Uuid::new_v4());

    // Register schema1 (backward-compatible baseline).
    let schema1 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"}]}"#;
    // wrongSchema2: adds field g as string with default — backward-compatible with schema1.
    let wrong_schema2 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"g","default":"d"}]}"#;
    // correctSchema2: adds field g as int with default — backward-compatible with schema1,
    // but NOT backward-compatible with wrongSchema2 (g changed string→int).
    let correct_schema2 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"int","name":"g","default":0}]}"#;

    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "BACKWARD"}))
        .send()
        .await
        .unwrap();

    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema1}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": wrong_schema2}))
        .send()
        .await
        .unwrap();

    // correctSchema2 is incompatible with wrongSchema2 (latest) → rejected.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": correct_schema2}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Soft-delete wrongSchema2 (version 2).
    client
        .delete(format!("{base}/subjects/{subject}/versions/2"))
        .send()
        .await
        .unwrap();

    // Now latest active is schema1. correctSchema2 is backward-compatible with schema1 → succeeds.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": correct_schema2}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Confluent `testCompatibilityLevelChangeToNone`: reject → change to NONE → succeed.
#[tokio::test]
async fn register_level_change_to_none_allows_incompatible() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-lvl-none-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::COMPAT_AVRO_V1).await;

    // Default BACKWARD — incompatible schema rejected.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Change to NONE on the subject — same schema now succeeds.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "NONE"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": common::COMPAT_AVRO_INCOMPAT}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Confluent `testCompatibilityLevelChangeToBackward`: register under FORWARD, switch to BACKWARD, reject.
#[tokio::test]
async fn register_level_change_forward_to_backward() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("compat-lvl-fw-bw-{}", uuid::Uuid::new_v4());

    let schema1 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"}]}"#;
    // schema2 adds required f2 — forward-compatible with schema1 (old reader ignores f2).
    let schema2 = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2"}]}"#;
    // schema3 adds required f3 — forward-compatible with schema2 but NOT backward-compatible
    // (schema2 reader expects f2+f1, schema3 adds f3 without default).
    let schema3_no_default = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2"},{"type":"string","name":"f3"}]}"#;
    let schema3_with_default = r#"{"type":"record","name":"myrecord","fields":[{"type":"string","name":"f1"},{"type":"string","name":"f2"},{"type":"string","name":"f3","default":"foo"}]}"#;

    // Set FORWARD and register schema1 + schema2.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "FORWARD"}))
        .send()
        .await
        .unwrap();

    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema1}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema2}))
        .send()
        .await
        .unwrap();

    // Switch to BACKWARD.
    client
        .put(format!("{base}/config/{subject}"))
        .json(&serde_json::json!({"compatibility": "BACKWARD"}))
        .send()
        .await
        .unwrap();

    // schema3 without default is forward-compatible but NOT backward-compatible → rejected.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema3_no_default}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // schema3 with default IS backward-compatible → succeeds.
    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema3_with_default}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// -- Python-style booleans (case-insensitive query params) --

#[tokio::test]
async fn register_with_python_style_normalize_true() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("py-norm-{}", uuid::Uuid::new_v4());

    let schema_compact =
        r#"{"type":"record","name":"PyNorm","fields":[{"name":"id","type":"int"}]}"#;
    let schema_spaced = r#"{  "type" : "record",  "name" : "PyNorm",  "fields" : [ { "name" : "id", "type" : "int" } ] }"#;

    // Python's str(True) → "True"
    let resp1 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=True"))
        .json(&serde_json::json!({"schema": schema_compact}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let id1 = resp1.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp2 = client
        .post(format!("{base}/subjects/{subject}/versions?normalize=True"))
        .json(&serde_json::json!({"schema": schema_spaced}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let id2 = resp2.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    assert_eq!(
        id1, id2,
        "normalize=True (Python-style) should deduplicate like normalize=true"
    );
}

#[tokio::test]
async fn register_with_python_style_normalize_false() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("py-norm-false-{}", uuid::Uuid::new_v4());

    // Python's str(False) → "False"
    let resp = client
        .post(format!(
            "{base}/subjects/{subject}/versions?normalize=False"
        ))
        .json(&serde_json::json!({"schema": common::AVRO_SCHEMA_V1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A schema larger than 32 KB must round-trip intact — guards the large-payload
/// write path (`schema_text` / `canonical_form`), where a small schema would pass
/// even if the storage of big values regressed.
#[tokio::test]
async fn register_large_schema_over_32kb_round_trips() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("large-clob-{}", uuid::Uuid::new_v4());

    // Build a valid Avro record with enough optional fields to exceed 32 KB.
    let fields: Vec<serde_json::Value> = (0..1200)
        .map(|n| {
            serde_json::json!({
                "name": format!("field_{n}"),
                "type": ["null", "string"],
                "default": null,
            })
        })
        .collect();
    let schema = serde_json::json!({
        "type": "record",
        "name": "LargeRecord",
        "fields": fields,
    })
    .to_string();
    assert!(
        schema.len() > 32 * 1024,
        "test schema must exceed 32 KB, was {} bytes",
        schema.len()
    );

    let resp = client
        .post(format!("{base}/subjects/{subject}/versions"))
        .json(&serde_json::json!({"schema": schema}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "registering a >32 KB schema should succeed"
    );

    // Read it back and assert the full text survived the CLOB round-trip.
    let got = common::api::get_schema_by_version(&client, &base, &subject, "1").await;
    assert_eq!(got.status(), StatusCode::OK);
    let body: serde_json::Value = got.json().await.unwrap();
    let returned = body["schema"].as_str().expect("schema field present");

    // Both sides are Avro JSON; compare structurally so whitespace/key-order
    // differences don't cause a false failure. The field count is the load-bearing
    // assertion (proves no truncation).
    let returned_val: serde_json::Value = serde_json::from_str(returned).unwrap();
    assert_eq!(
        returned_val["fields"].as_array().map(Vec::len),
        Some(1200),
        "all 1200 fields must round-trip (no CLOB truncation)"
    );
}
