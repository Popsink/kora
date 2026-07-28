//! Integration tests for database migrations.

mod common;

#[tokio::test]
async fn migrations_create_all_tables() {
    let pool = common::pool().await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("should query tables");

    let table_names: Vec<&str> = tables.iter().map(String::as_str).collect();
    assert!(table_names.contains(&"subjects"), "missing subjects table");
    assert!(
        table_names.contains(&"schema_contents"),
        "missing schema_contents table"
    );
    assert!(
        table_names.contains(&"schema_versions"),
        "missing schema_versions table"
    );
    assert!(
        table_names.contains(&"schema_references"),
        "missing schema_references table"
    );
    assert!(table_names.contains(&"config"), "missing config table");
}

#[tokio::test]
async fn migrations_insert_global_config() {
    let pool = common::pool().await;

    let (level, mode): (String, String) =
        sqlx::query_as("SELECT compatibility_level, mode FROM config WHERE subject IS NULL")
            .fetch_one(&pool)
            .await
            .expect("global config row should exist");

    assert_eq!(level, "BACKWARD");
    assert_eq!(mode, "READWRITE");
}
