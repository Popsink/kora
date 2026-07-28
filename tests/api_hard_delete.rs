//! Integration tests for hard-deleting subjects and versions
//! (DELETE /subjects/{subject}?permanent=true, DELETE /subjects/{subject}/versions/{version}?permanent=true).

mod common;

use reqwest::StatusCode;

#[tokio::test]
async fn hard_delete_subject_after_soft_delete_succeeds() {
    let base = common::spawn_server().await;
    let pool = common::pool().await;
    let client = reqwest::Client::new();
    let subject = format!("hard-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V2).await;
    common::api::delete_subject(&client, &base, &subject).await;

    let resp = common::api::hard_delete_subject(&client, &base, &subject).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let versions: Vec<i32> = resp.json().await.unwrap();
    assert_eq!(versions, vec![1, 2]);

    let count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subjects WHERE name = $1")
        .bind(&subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "subject should be permanently removed");
}

#[tokio::test]
async fn hard_delete_version_after_soft_delete_succeeds() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("hard-ver-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V2).await;
    common::api::delete_version(&client, &base, &subject, "2").await;

    let resp = common::api::hard_delete_version(&client, &base, &subject, 2).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: i32 = resp.json().await.unwrap();
    assert_eq!(body, 2);

    let versions =
        common::api::list_versions(&client, &base, &subject, common::INCLUDE_DELETED).await;
    assert_eq!(
        versions,
        vec![1],
        "hard-deleted version should not appear even with ?deleted=true"
    );
}

#[tokio::test]
async fn hard_delete_subject_without_soft_delete_returns_40405() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("hard-nosoft-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    let resp = common::api::hard_delete_subject(&client, &base, &subject).await;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40405);
}

#[tokio::test]
async fn hard_delete_version_without_soft_delete_returns_40407() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("hard-ver-nosoft-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;

    let resp = common::api::hard_delete_version(&client, &base, &subject, 1).await;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40407);
}

#[tokio::test]
async fn hard_delete_nonexistent_subject_returns_40401() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();

    let resp = common::api::hard_delete_subject(&client, &base, "never-existed").await;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40401);
}

#[tokio::test]
async fn hard_delete_subject_twice_returns_40401() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("hard-twice-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    common::api::delete_subject(&client, &base, &subject).await;

    let resp = common::api::hard_delete_subject(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = common::api::hard_delete_subject(&client, &base, &subject).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40401);
}

// -- Python-style booleans (case-insensitive query params) --

#[tokio::test]
async fn hard_delete_subject_accepts_python_style_permanent_true() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("py-hard-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    common::api::delete_subject(&client, &base, &subject).await;

    // Python's str(True) → "True"
    let resp = client
        .delete(format!("{base}/subjects/{subject}?permanent=True"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn hard_delete_version_accepts_python_style_permanent_true() {
    let base = common::spawn_server().await;
    let client = reqwest::Client::new();
    let subject = format!("py-hard-ver-{}", uuid::Uuid::new_v4());

    common::api::register_schema(&client, &base, &subject, common::AVRO_SCHEMA_V1).await;
    common::api::delete_version(&client, &base, &subject, "1").await;

    // Python's str(True) → "True"
    let resp = client
        .delete(format!(
            "{base}/subjects/{subject}/versions/1?permanent=True"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
