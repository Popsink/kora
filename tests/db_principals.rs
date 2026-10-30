//! Integration tests for principal persistence and the bootstrap admin.

mod common;

use kora::auth::{self, Access, Role, Rule};
use kora::storage::types::{NewPrincipal, PrincipalUpdate};

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().as_simple())
}

#[tokio::test]
async fn principal_crud_round_trip() {
    let storage = common::storage().await;
    let name = unique("tenant");
    let rules = vec![Rule {
        pattern: "a.*".into(),
        access: Access::ReadWrite,
    }];

    let created = storage
        .create_principal(&NewPrincipal {
            name: &name,
            password_hash: "hash-1",
            role: Role::Tenant,
            rules: &rules,
        })
        .await
        .unwrap()
        .expect("principal should be created");
    assert_eq!(created.role, Role::Tenant);
    assert_eq!(created.rules, rules);
    assert!(created.created_at.ends_with('Z'));

    // Duplicate name → None.
    let dup = storage
        .create_principal(&NewPrincipal {
            name: &name,
            password_hash: "hash-2",
            role: Role::Admin,
            rules: &[],
        })
        .await
        .unwrap();
    assert!(dup.is_none());

    // Partial update keeps untouched fields.
    let updated = storage
        .update_principal(
            &name,
            &PrincipalUpdate {
                password_hash: Some("hash-3"),
                ..PrincipalUpdate::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.password_hash, "hash-3");
    assert_eq!(updated.rules, rules);

    let listed = storage.list_principals().await.unwrap();
    assert!(listed.iter().any(|p| p.name == name));

    assert_eq!(
        storage.delete_principal(&name).await.unwrap(),
        Some(created.id)
    );
    assert!(storage.find_principal(&name).await.unwrap().is_none());
    assert!(storage.delete_principal(&name).await.unwrap().is_none());
}

#[tokio::test]
async fn bootstrap_admin_creates_then_reconciles() {
    let storage = common::storage().await;
    let name = unique("admin");

    assert!(
        auth::bootstrap_admin(&storage, &name, "pw-1")
            .await
            .unwrap()
    );
    // Same password again → no change.
    assert!(
        !auth::bootstrap_admin(&storage, &name, "pw-1")
            .await
            .unwrap()
    );

    // Demoted and rotated out of band → env wins on next boot.
    storage
        .update_principal(
            &name,
            &PrincipalUpdate {
                role: Some(Role::Tenant),
                ..PrincipalUpdate::default()
            },
        )
        .await
        .unwrap();
    assert!(
        auth::bootstrap_admin(&storage, &name, "pw-2")
            .await
            .unwrap()
    );
    let rec = storage.find_principal(&name).await.unwrap().unwrap();
    assert_eq!(rec.role, Role::Admin);
    assert!(auth::verify_password("pw-2", &rec.password_hash));

    storage.delete_principal(&name).await.unwrap();
}
