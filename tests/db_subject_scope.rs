//! Integration tests for the read-scope pushdown in storage listings.

mod common;

use kora::auth::SubjectScope;

/// Register `schema` under `subject` through the HTTP API and return its ID.
async fn register(base: &str, subject: &str, schema: &str) -> i64 {
    common::api::register_schema(&reqwest::Client::new(), base, subject, schema).await
}

#[tokio::test]
async fn listings_only_return_subjects_in_scope() {
    let base = common::spawn_server().await;
    let storage = common::storage().await;
    let tag = uuid::Uuid::new_v4().as_simple().to_string();

    // `_` must be matched literally, not as a LIKE wildcard.
    let a_prefix = format!("a_{tag}.");
    let a1 = format!("{a_prefix}x-value");
    let lookalike = format!("aX{tag}.x-value");
    let exact = format!("legacy-{tag}");
    let other = format!("b-{tag}.x-value");

    let schema = common::unique_avro_schema();
    let id = register(&base, &a1, &schema).await;
    register(&base, &lookalike, &schema).await;
    register(&base, &exact, &schema).await;
    register(&base, &other, &schema).await;

    let scope = SubjectScope {
        names: vec![exact.clone()],
        prefixes: vec![a_prefix.clone()],
    };

    let subjects = storage
        .list_subjects(false, false, None, Some(&scope), 0, -1)
        .await
        .unwrap();
    assert_eq!(subjects, vec![a1.clone(), exact.clone()]);

    // Intersected with a subjectPrefix filter.
    let schemas = storage
        .list_schemas(false, false, Some(&a_prefix), Some(&scope), 0, -1)
        .await
        .unwrap();
    assert_eq!(
        schemas
            .iter()
            .map(|s| s.subject.as_str())
            .collect::<Vec<_>>(),
        vec![a1.as_str()]
    );

    let by_id = storage
        .find_subjects_by_schema_id(id, false, None, Some(&scope), 0, -1)
        .await
        .unwrap();
    assert_eq!(by_id, vec![a1.clone(), exact.clone()]);

    let versions = storage
        .find_versions_by_schema_id(id, false, Some(&exact), Some(&scope), 0, -1)
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].subject, exact);

    assert!(storage.schema_readable(id, &scope).await.unwrap());
    let nothing = SubjectScope {
        names: vec![],
        prefixes: vec![format!("zzz-{tag}")],
    };
    assert!(!storage.schema_readable(id, &nothing).await.unwrap());

    // An empty prefix (rule `*`) matches every subject.
    let everything = SubjectScope {
        names: vec![],
        prefixes: vec![String::new()],
    };
    let all = storage
        .find_subjects_by_schema_id(id, false, None, Some(&everything), 0, -1)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
}
