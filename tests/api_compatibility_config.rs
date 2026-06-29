//! Integration tests for compatibility configuration CRUD (Story 4.1).
//!
//! Tests that mutate the shared global config row are marked `#[serial]`
//! to prevent race conditions across parallel test execution.

mod common;

use kora::api::compatibility::COMPATIBILITY_LEVELS;
use reqwest::{Client, StatusCode};
use serial_test::serial;

// -- Global compatibility --

#[tokio::test]
#[serial]
async fn get_global_compatibility_returns_backward_default() {
    let base = common::spawn_server().await;
    let client = Client::new();

    let resp = common::api::get_global_compatibility(&client, &base).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "BACKWARD");
}

#[tokio::test]
#[serial]
async fn set_global_compatibility_updates_level() {
    let base = common::spawn_server().await;
    let client = Client::new();

    let resp = common::api::set_global_compatibility(&client, &base, "FULL").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibility"], "FULL");

    // Verify via GET
    let resp = common::api::get_global_compatibility(&client, &base).await;
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "FULL");

    // Restore default
    common::api::set_global_compatibility(&client, &base, "BACKWARD").await;
}

#[tokio::test]
async fn set_global_compatibility_rejects_invalid_level() {
    let base = common::spawn_server().await;
    let client = Client::new();

    let resp = common::api::set_global_compatibility(&client, &base, "INVALID").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 42203);
}

#[tokio::test]
#[serial]
async fn set_global_compatibility_accepts_all_valid_levels() {
    let base = common::spawn_server().await;
    let client = Client::new();

    for level in COMPATIBILITY_LEVELS {
        let resp = common::api::set_global_compatibility(&client, &base, level).await;
        assert_eq!(resp.status(), StatusCode::OK, "should accept level {level}");

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["compatibility"], *level);
    }

    // Restore default
    common::api::set_global_compatibility(&client, &base, "BACKWARD").await;
}

#[tokio::test]
#[serial]
async fn get_global_compatibility_accepts_default_to_global_param() {
    let base = common::spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/config?defaultToGlobal=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "BACKWARD");
}

#[tokio::test]
#[serial]
async fn delete_global_compatibility_resets_to_backward() {
    let base = common::spawn_server().await;
    let client = Client::new();

    common::api::set_global_compatibility(&client, &base, "FULL").await;

    let resp = client
        .delete(format!("{base}/config"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = common::api::get_global_compatibility(&client, &base).await;
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "BACKWARD");
}

// -- Per-subject compatibility --

#[tokio::test]
#[serial]
async fn get_subject_compatibility_without_config_returns_40408() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    // Without defaultToGlobal → 40408.
    let resp = common::api::get_subject_compatibility(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40408);

    // With defaultToGlobal=true → falls back to global.
    let resp = client
        .get(format!("{base}/config/{subject}?defaultToGlobal=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "BACKWARD");
}

#[tokio::test]
async fn set_subject_compatibility_sets_override() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    let resp = common::api::set_subject_compatibility(&client, &base, &subject, "NONE").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibility"], "NONE");

    // Verify via GET
    let resp = common::api::get_subject_compatibility(&client, &base, &subject).await;
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "NONE");
}

#[tokio::test]
#[serial]
async fn delete_subject_compatibility_returns_previous_level() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    // Set per-subject config to NONE.
    common::api::set_subject_compatibility(&client, &base, &subject, "NONE").await;

    // Delete per-subject config → returns previous Config object.
    let resp = common::api::delete_subject_compatibility(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "NONE");

    // Verify GET now returns 40408 (no per-subject config).
    let resp = common::api::get_subject_compatibility(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_subject_compatibility_without_config_returns_40401() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-noconfig-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    // No per-subject config set → DELETE returns 40401 (Confluent: subjectNotFoundException).
    let resp = common::api::delete_subject_compatibility(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40401);
}

#[tokio::test]
async fn subject_compatibility_nonexistent_returns_40408_not_40401() {
    let base = common::spawn_server().await;
    let client = Client::new();

    // Confluent does not check subject existence — only config existence.
    // GET on nonexistent subject with no config → 40408.
    let resp = common::api::get_subject_compatibility(&client, &base, "nonexistent").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40408);

    // PUT on nonexistent subject succeeds (Confluent allows config on any subject name).
    let resp = common::api::set_subject_compatibility(&client, &base, "nonexistent", "FULL").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Clean up the config we just set.
    let resp = common::api::delete_subject_compatibility(&client, &base, "nonexistent").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "FULL");

    // DELETE on nonexistent subject with no config → 40401 (Confluent: subjectNotFoundException).
    let resp = common::api::delete_subject_compatibility(&client, &base, "nonexistent").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40401);
}

#[tokio::test]
async fn set_subject_compatibility_rejects_invalid_level() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    let resp = common::api::set_subject_compatibility(&client, &base, &subject, "BOGUS").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 42203);
}

// -- Override priority --

#[tokio::test]
#[serial]
async fn get_subject_compatibility_returns_override_not_global() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    // Set global to FULL, subject to NONE
    common::api::set_global_compatibility(&client, &base, "FULL").await;
    common::api::set_subject_compatibility(&client, &base, &subject, "NONE").await;

    // Subject should return its own override, not the global
    let resp = common::api::get_subject_compatibility(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "NONE");

    // Restore global default
    common::api::set_global_compatibility(&client, &base, "BACKWARD").await;
}

// -- Python-style booleans (case-insensitive query params) --

#[tokio::test]
#[serial]
async fn get_global_compatibility_accepts_python_style_default_to_global() {
    let base = common::spawn_server().await;
    let client = Client::new();

    // Python's str(True) → "True"
    let resp = client
        .get(format!("{base}/config?defaultToGlobal=True"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn get_subject_compatibility_accepts_python_style_default_to_global() {
    let base = common::spawn_server().await;
    let client = Client::new();
    let subject = format!("py-compat-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    // Python's str(True) → "True"
    let resp = client
        .get(format!("{base}/config/{subject}?defaultToGlobal=True"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["compatibilityLevel"], "BACKWARD");
}

// -- Declarative startup default (issue #47) --
//
// `storage::apply_startup_config` is what the startup path (main.rs) calls when
// `DEFAULT_COMPATIBILITY` is set; it delegates to `reconcile_global_level`. These
// tests mutate the shared global config row, so — like the global `/config` tests
// above — they are `#[serial]` and restore `BACKWARD`. This binary is the only one
// that touches the global row, so `#[serial]` is sufficient to serialize them.

/// A configured default is the source of truth: startup reconciliation overwrites
/// whatever global level was stored (e.g. a prior runtime change).
#[tokio::test]
#[serial]
async fn apply_startup_config_overrides_stored_global_level() {
    let storage = common::storage().await;

    // A previously-stored level (e.g. set by an operator via PUT /config).
    storage.set_global_level("FORWARD", false).await.unwrap();

    let applied = storage.apply_startup_config(Some("NONE")).await.unwrap();
    assert_eq!(applied.as_deref(), Some("NONE"));

    let stored = storage.get_global_level().await.unwrap();
    assert_eq!(stored, "NONE", "the declared default must win on startup");

    // Restore the suite default.
    storage.set_global_level("BACKWARD", false).await.unwrap();
}

/// With no configured default, startup is a no-op and leaves the stored level alone.
#[tokio::test]
#[serial]
async fn apply_startup_config_without_default_is_noop() {
    let storage = common::storage().await;

    storage.set_global_level("FULL", false).await.unwrap();

    let applied = storage.apply_startup_config(None).await.unwrap();
    assert_eq!(applied, None);

    let stored = storage.get_global_level().await.unwrap();
    assert_eq!(
        stored, "FULL",
        "no declared default must leave the stored level untouched"
    );

    // Restore the suite default.
    storage.set_global_level("BACKWARD", false).await.unwrap();
}

/// Startup reconciliation never clobbers per-subject overrides
/// (the issue's explicit requirement).
#[tokio::test]
#[serial]
async fn apply_startup_config_preserves_subject_overrides() {
    let storage = common::storage().await;
    let subject = format!("startup-compat-{}", uuid::Uuid::new_v4());

    storage
        .set_subject_level(&subject, "FORWARD", false)
        .await
        .unwrap();

    storage.apply_startup_config(Some("FULL")).await.unwrap();

    let subject_level = storage.get_subject_level(&subject).await.unwrap();
    assert_eq!(
        subject_level.as_deref(),
        Some("FORWARD"),
        "per-subject override must survive startup reconciliation"
    );

    let global = storage.get_global_level().await.unwrap();
    assert_eq!(global, "FULL");

    // Restore the suite default.
    storage.set_global_level("BACKWARD", false).await.unwrap();
}

/// Reconcile changes only the compatibility level — it leaves the global
/// `normalize` flag intact (`DEFAULT_COMPATIBILITY` governs the level only).
#[tokio::test]
#[serial]
async fn reconcile_global_level_preserves_normalize_flag() {
    let storage = common::storage().await;

    // Configure the global row with normalize = true via the regular API path.
    storage.set_global_level("BACKWARD", true).await.unwrap();

    storage.reconcile_global_level("FULL").await.unwrap();

    let normalize = storage.get_global_normalize().await.unwrap();
    assert!(normalize, "reconcile must not reset the normalize flag");

    // Restore the suite default.
    storage.set_global_level("BACKWARD", false).await.unwrap();
}
