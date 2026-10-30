//! Integration tests for authentication and the subject-scoped ACL (#91).
//!
//! One test (or group) per row of the enforcement table, plus the admin API,
//! cache eviction and the rejection metric. Every test creates its own
//! principals and subject prefixes: the database is shared across tests.

mod common;

use common::{AuthServer, Creds};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::{Value, json};
use serial_test::file_serial;

// -- Helpers --

struct Api {
    client: Client,
    base: String,
}

impl Api {
    fn new(server: &AuthServer) -> Self {
        Self {
            client: Client::new(),
            base: server.base.clone(),
        }
    }

    fn req(&self, method: Method, path: &str, who: Option<&Creds>) -> RequestBuilder {
        let rb = self.client.request(method, format!("{}{path}", self.base));
        match who {
            Some(c) => rb.basic_auth(&c.name, Some(&c.password)),
            None => rb,
        }
    }

    async fn get(&self, path: &str, who: &Creds) -> Response {
        self.req(Method::GET, path, Some(who)).send().await.unwrap()
    }

    async fn send(&self, method: Method, path: &str, who: &Creds, body: Option<Value>) -> Response {
        let rb = self.req(method, path, Some(who));
        match body {
            Some(b) => rb.json(&b),
            None => rb,
        }
        .send()
        .await
        .unwrap()
    }

    async fn register(&self, who: &Creds, subject: &str, schema: &str) -> Response {
        self.send(
            Method::POST,
            &format!("/subjects/{subject}/versions"),
            who,
            Some(json!({ "schema": schema })),
        )
        .await
    }

    /// Register and return the schema ID, asserting success.
    async fn register_ok(&self, who: &Creds, subject: &str, schema: &str) -> i64 {
        let resp = self.register(who, subject, schema).await;
        assert_eq!(resp.status(), StatusCode::OK, "register {subject}");
        resp.json::<Value>().await.unwrap()["id"].as_i64().unwrap()
    }

    /// Create a tenant with `rules` through the admin API.
    async fn tenant(&self, admin: &Creds, rules: Value) -> Creds {
        let creds = Creds::unique("tenant");
        let resp = self
            .send(
                Method::POST,
                "/admin/principals",
                admin,
                Some(json!({
                    "name": creds.name,
                    "password": creds.password,
                    "role": "tenant",
                    "rules": rules,
                })),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        creds
    }
}

/// A unique subject prefix for this test (`p<uuid>.`).
fn prefix() -> String {
    format!("p{}.", uuid::Uuid::new_v4().as_simple())
}

async fn assert_forbidden(resp: Response, what: &str) {
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{what}");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40301, "{what}");
}

async fn json_strings(resp: Response) -> Vec<String> {
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.unwrap()
}

// -- Authentication --

#[tokio::test]
async fn missing_or_wrong_credentials_get_401_except_health_and_metrics() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let wrong = Creds {
        name: server.admin.name.clone(),
        password: "wrong".into(),
    };
    let unknown = Creds::unique("nobody");

    let routes = [
        (Method::GET, "/"),
        (Method::GET, "/subjects"),
        (Method::GET, "/schemas"),
        (Method::GET, "/schemas/types"),
        (Method::GET, "/schemas/ids/1"),
        (Method::GET, "/schemas/ids/1/subjects"),
        (Method::GET, "/subjects/x/versions"),
        (Method::POST, "/subjects/x/versions"),
        (Method::DELETE, "/subjects/x"),
        (Method::POST, "/compatibility/subjects/x/versions"),
        (Method::GET, "/config"),
        (Method::PUT, "/config/x"),
        (Method::GET, "/mode"),
        (Method::GET, "/admin/principals"),
    ];
    for (method, path) in routes {
        for who in [None, Some(&wrong), Some(&unknown)] {
            let resp = api.req(method.clone(), path, who).send().await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} as {who:?}"
            );
            assert!(
                resp.headers()
                    .get("www-authenticate")
                    .is_some_and(|v| v.to_str().unwrap().starts_with("Basic")),
                "{method} {path}: missing basic challenge"
            );
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["error_code"], 40101);
        }
    }

    for path in ["/health", "/metrics"] {
        let resp = api.req(Method::GET, path, None).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path} must stay open");
    }

    let resp = api.get("/subjects", &server.admin).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// -- Row 1: subject-scoped routes --

#[tokio::test]
async fn subject_routes_require_read_or_read_write() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b, ro) = (prefix(), prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([
                { "pattern": format!("{a}*"), "access": "read_write" },
                { "pattern": format!("{ro}*"), "access": "read" },
            ]),
        )
        .await;
    let (own, other, readonly) = (
        format!("{a}x-value"),
        format!("{b}x-value"),
        format!("{ro}x-value"),
    );
    let schema = common::unique_avro_schema();
    api.register_ok(&server.admin, &other, &schema).await;
    api.register_ok(&server.admin, &readonly, &schema).await;

    // Own subject: full read/write.
    api.register_ok(&tenant, &own, &schema).await;
    for path in [
        format!("/subjects/{own}/versions"),
        format!("/subjects/{own}/versions/1"),
        format!("/subjects/{own}/versions/latest/schema"),
        format!("/subjects/{own}/versions/1/referencedby"),
    ] {
        assert_eq!(
            api.get(&path, &tenant).await.status(),
            StatusCode::OK,
            "{path}"
        );
    }
    let body = Some(json!({ "schema": schema }));
    for path in [
        format!("/subjects/{own}"),
        format!("/compatibility/subjects/{own}/versions"),
    ] {
        let resp = api.send(Method::POST, &path, &tenant, body.clone()).await;
        assert_eq!(resp.status(), StatusCode::OK, "POST {path}");
    }

    // Another tenant's subject: nothing, whether or not it exists.
    let missing = format!("{b}does-not-exist");
    for subject in [&other, &missing] {
        assert_forbidden(
            api.get(&format!("/subjects/{subject}/versions"), &tenant)
                .await,
            "GET other versions",
        )
        .await;
        assert_forbidden(
            api.get(&format!("/subjects/{subject}/versions/1/schema"), &tenant)
                .await,
            "GET other schema",
        )
        .await;
        assert_forbidden(
            api.send(
                Method::POST,
                &format!("/subjects/{subject}"),
                &tenant,
                body.clone(),
            )
            .await,
            "lookup other",
        )
        .await;
        assert_forbidden(
            api.register(&tenant, subject, &schema).await,
            "register other",
        )
        .await;
        assert_forbidden(
            api.send(
                Method::DELETE,
                &format!("/subjects/{subject}"),
                &tenant,
                None,
            )
            .await,
            "delete other",
        )
        .await;
        assert_forbidden(
            api.send(
                Method::DELETE,
                &format!("/subjects/{subject}/versions/1"),
                &tenant,
                None,
            )
            .await,
            "delete other version",
        )
        .await;
        assert_forbidden(
            api.send(
                Method::POST,
                &format!("/compatibility/subjects/{subject}/versions"),
                &tenant,
                body.clone(),
            )
            .await,
            "compat other",
        )
        .await;
    }

    // Read-only rule: reads and lookups pass, writes do not.
    assert_eq!(
        api.get(&format!("/subjects/{readonly}/versions"), &tenant)
            .await
            .status(),
        StatusCode::OK
    );
    let resp = api
        .send(
            Method::POST,
            &format!("/subjects/{readonly}"),
            &tenant,
            body.clone(),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_forbidden(
        api.register(&tenant, &readonly, &schema).await,
        "register ro",
    )
    .await;
    assert_forbidden(
        api.send(
            Method::DELETE,
            &format!("/subjects/{readonly}"),
            &tenant,
            None,
        )
        .await,
        "delete ro",
    )
    .await;

    // Own subject can be deleted.
    let resp = api
        .send(Method::DELETE, &format!("/subjects/{own}"), &tenant, None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn subject_config_writable_by_tenant_but_subject_mode_admin_only() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b, ro) = (prefix(), prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([
                { "pattern": format!("{a}*"), "access": "read_write" },
                { "pattern": format!("{ro}*"), "access": "read" },
            ]),
        )
        .await;
    let (own, other, readonly) = (format!("{a}x"), format!("{b}x"), format!("{ro}x"));
    let full = Some(json!({ "compatibility": "FULL" }));

    // /config/{subject}: read with read, write with read_write.
    let resp = api
        .send(
            Method::PUT,
            &format!("/config/{own}"),
            &tenant,
            full.clone(),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        api.get(&format!("/config/{own}"), &tenant).await.status(),
        StatusCode::OK
    );
    let resp = api
        .send(Method::DELETE, &format!("/config/{own}"), &tenant, None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        api.get(&format!("/config/{readonly}?defaultToGlobal=true"), &tenant)
            .await
            .status(),
        StatusCode::OK
    );
    for subject in [&other, &readonly] {
        assert_forbidden(
            api.send(
                Method::PUT,
                &format!("/config/{subject}"),
                &tenant,
                full.clone(),
            )
            .await,
            "PUT config",
        )
        .await;
        assert_forbidden(
            api.send(Method::DELETE, &format!("/config/{subject}"), &tenant, None)
                .await,
            "DELETE config",
        )
        .await;
    }
    assert_forbidden(
        api.get(&format!("/config/{other}"), &tenant).await,
        "GET other config",
    )
    .await;

    // /mode/{subject}: tenants read their own, only admins write.
    assert_eq!(
        api.get(&format!("/mode/{own}?defaultToGlobal=true"), &tenant)
            .await
            .status(),
        StatusCode::OK
    );
    assert_forbidden(
        api.get(&format!("/mode/{other}?defaultToGlobal=true"), &tenant)
            .await,
        "GET other mode",
    )
    .await;
    let readonly_mode = Some(json!({ "mode": "READONLY" }));
    assert_forbidden(
        api.send(
            Method::PUT,
            &format!("/mode/{own}"),
            &tenant,
            readonly_mode.clone(),
        )
        .await,
        "PUT own mode",
    )
    .await;
    assert_forbidden(
        api.send(
            Method::DELETE,
            &format!("/mode/{own}?recursive=true"),
            &tenant,
            None,
        )
        .await,
        "DELETE own mode",
    )
    .await;
    let resp = api
        .send(
            Method::PUT,
            &format!("/mode/{own}"),
            &server.admin,
            readonly_mode,
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = api
        .send(Method::DELETE, &format!("/mode/{own}"), &server.admin, None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// -- Row 2: GET /subjects --

#[tokio::test]
async fn list_subjects_is_filtered_to_readable_subjects() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let legacy = format!("legacy-{}", uuid::Uuid::new_v4().as_simple());
    let tenant = api
        .tenant(
            &server.admin,
            json!([
                { "pattern": format!("{a}*"), "access": "read_write" },
                { "pattern": legacy, "access": "read" },
            ]),
        )
        .await;
    let schema = common::unique_avro_schema();
    for s in [format!("{a}x-value"), format!("{b}x-value"), legacy.clone()] {
        api.register_ok(&server.admin, &s, &schema).await;
    }
    // Soft-deleted subject of the tenant shows up only with deleted=true.
    let gone = format!("{a}gone-value");
    api.register_ok(&server.admin, &gone, &schema).await;
    api.send(
        Method::DELETE,
        &format!("/subjects/{gone}"),
        &server.admin,
        None,
    )
    .await;

    let mine: Vec<String> = json_strings(api.get("/subjects", &tenant).await)
        .await
        .into_iter()
        .filter(|s| s.starts_with(&a) || s.starts_with(&b) || *s == legacy)
        .collect();
    assert_eq!(mine, vec![legacy.clone(), format!("{a}x-value")]);

    let all = json_strings(api.get("/subjects?deleted=true", &tenant).await).await;
    assert!(all.contains(&gone));
    assert!(all.iter().all(|s| !s.starts_with(&b)));

    // Pagination applies after the filter.
    let page = json_strings(
        api.get(&format!("/subjects?subjectPrefix={a}&limit=1"), &tenant)
            .await,
    )
    .await;
    assert_eq!(page, vec![format!("{a}x-value")]);

    // Admins see everything.
    let admin_view = json_strings(api.get("/subjects", &server.admin).await).await;
    assert!(admin_view.contains(&format!("{b}x-value")));
}

// -- Row 3: GET /schemas --

#[tokio::test]
async fn list_schemas_is_filtered_and_intersected_with_subject_prefix() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{a}*"), "access": "read" }]),
        )
        .await;
    let schema = common::unique_avro_schema();
    api.register_ok(&server.admin, &format!("{a}x-value"), &schema)
        .await;
    api.register_ok(&server.admin, &format!("{b}x-value"), &schema)
        .await;

    let subjects_of = |v: Vec<Value>| -> Vec<String> {
        v.iter()
            .map(|s| s["subject"].as_str().unwrap().to_owned())
            .filter(|s| s.starts_with(&a) || s.starts_with(&b))
            .collect()
    };

    let resp = api.get("/schemas", &tenant).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        subjects_of(resp.json().await.unwrap()),
        vec![format!("{a}x-value")]
    );

    let resp = api
        .get(&format!("/schemas?subjectPrefix={b}"), &tenant)
        .await;
    assert!(subjects_of(resp.json().await.unwrap()).is_empty());

    let resp = api
        .get(
            &format!("/schemas?subjectPrefix={a}&latestOnly=true"),
            &tenant,
        )
        .await;
    assert_eq!(
        subjects_of(resp.json().await.unwrap()),
        vec![format!("{a}x-value")]
    );
}

// -- Row 4: /schemas/ids/{id}/subjects and /versions --

#[tokio::test]
async fn schema_id_cross_refs_only_return_readable_pairs() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{a}*"), "access": "read_write" }]),
        )
        .await;
    let shared = common::unique_avro_schema();
    let id = api
        .register_ok(&server.admin, &format!("{a}x-value"), &shared)
        .await;
    api.register_ok(&server.admin, &format!("{b}x-value"), &shared)
        .await;
    let b_only = api
        .register_ok(
            &server.admin,
            &format!("{b}y-value"),
            &common::unique_avro_schema(),
        )
        .await;

    let subjects = json_strings(
        api.get(&format!("/schemas/ids/{id}/subjects"), &tenant)
            .await,
    )
    .await;
    assert_eq!(subjects, vec![format!("{a}x-value")]);

    let resp = api
        .get(&format!("/schemas/ids/{id}/versions"), &tenant)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let versions: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(
        versions,
        vec![json!({ "subject": format!("{a}x-value"), "version": 1 })]
    );

    // `?subject=` naming another tenant's subject yields nothing.
    let filtered = json_strings(
        api.get(
            &format!("/schemas/ids/{id}/subjects?subject={b}x-value"),
            &tenant,
        )
        .await,
    )
    .await;
    assert!(filtered.is_empty());

    for path in [
        format!("/schemas/ids/{b_only}/subjects"),
        format!("/schemas/ids/{b_only}/versions"),
    ] {
        let resp = api.get(&path, &tenant).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error_code"], 40403);
    }

    let admin_view = json_strings(
        api.get(&format!("/schemas/ids/{id}/subjects"), &server.admin)
            .await,
    )
    .await;
    assert_eq!(admin_view.len(), 2);
}

// -- Row 5: /schemas/ids/{id} and /schemas/ids/{id}/schema --

#[tokio::test]
async fn schema_by_id_requires_a_readable_subject_and_shared_ids_resolve() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let tenant_a = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{a}*"), "access": "read_write" }]),
        )
        .await;
    let tenant_b = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{b}*"), "access": "read_write" }]),
        )
        .await;

    // Identical content registered by both tenants shares one ID; both resolve it.
    let shared = common::unique_avro_schema();
    let id_a = api
        .register_ok(&tenant_a, &format!("{a}x-value"), &shared)
        .await;
    let id_b = api
        .register_ok(&tenant_b, &format!("{b}x-value"), &shared)
        .await;
    assert_eq!(id_a, id_b);
    for who in [&tenant_a, &tenant_b] {
        for _ in 0..2 {
            // Second round is served from the per-principal decision cache.
            assert_eq!(
                api.get(&format!("/schemas/ids/{id_a}"), who).await.status(),
                StatusCode::OK
            );
            assert_eq!(
                api.get(&format!("/schemas/ids/{id_a}/schema"), who)
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }

    // An ID registered only under b.* is invisible to A — same answer as a
    // non-existent ID, so A cannot enumerate other tenants' IDs.
    let b_only = api
        .register_ok(
            &tenant_b,
            &format!("{b}y-value"),
            &common::unique_avro_schema(),
        )
        .await;
    for id in [b_only, i64::from(i32::MAX)] {
        for path in [
            format!("/schemas/ids/{id}"),
            format!("/schemas/ids/{id}/schema"),
        ] {
            let resp = api.get(&path, &tenant_a).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["error_code"], 40403);
        }
    }

    // B registering the same content later makes it readable right away (only
    // positive decisions are cached).
    let late = common::unique_avro_schema();
    let late_id = api
        .register_ok(&tenant_a, &format!("{a}late-value"), &late)
        .await;
    assert_eq!(
        api.get(&format!("/schemas/ids/{late_id}"), &tenant_b)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    api.register_ok(&tenant_b, &format!("{b}late-value"), &late)
        .await;
    assert_eq!(
        api.get(&format!("/schemas/ids/{late_id}"), &tenant_b)
            .await
            .status(),
        StatusCode::OK
    );
}

// -- Row 6: registration with references --

#[tokio::test]
async fn references_must_point_to_readable_subjects() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{a}*"), "access": "read_write" }]),
        )
        .await;
    let referenced = common::unique_avro_schema();
    api.register_ok(&server.admin, &format!("{a}ref"), &referenced)
        .await;
    api.register_ok(&server.admin, &format!("{b}ref"), &referenced)
        .await;

    let referencing = common::unique_avro_schema();
    let with_ref = |subject: String| {
        json!({
            "schema": referencing,
            "references": [{ "name": "ref", "subject": subject, "version": 1 }],
        })
    };

    assert_forbidden(
        api.send(
            Method::POST,
            &format!("/subjects/{a}top-value/versions"),
            &tenant,
            Some(with_ref(format!("{b}ref"))),
        )
        .await,
        "reference to another tenant",
    )
    .await;

    let resp = api
        .send(
            Method::POST,
            &format!("/subjects/{a}top-value/versions"),
            &tenant,
            Some(with_ref(format!("{a}ref"))),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// -- Row 7: global /config and /mode --

#[tokio::test]
#[file_serial]
async fn global_config_and_mode_readable_by_all_writable_by_admin() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{}*", prefix()), "access": "read_write" }]),
        )
        .await;

    let resp = api.get("/config", &tenant).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let level = resp.json::<Value>().await.unwrap()["compatibilityLevel"].clone();
    let resp = api.get("/mode", &tenant).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mode = resp.json::<Value>().await.unwrap()["mode"].clone();

    for (method, path, body) in [
        (
            Method::PUT,
            "/config",
            Some(json!({ "compatibility": level })),
        ),
        (Method::DELETE, "/config", None),
        (Method::PUT, "/mode", Some(json!({ "mode": mode }))),
        (Method::DELETE, "/mode", None),
    ] {
        assert_forbidden(
            api.send(method.clone(), path, &tenant, body).await,
            &format!("{method} {path}"),
        )
        .await;
    }

    // Admin writes (re-applying the current values to leave global state as is).
    let resp = api
        .send(
            Method::PUT,
            "/config",
            &server.admin,
            Some(json!({ "compatibility": level })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = api
        .send(
            Method::PUT,
            "/mode",
            &server.admin,
            Some(json!({ "mode": mode })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// -- Admin API --

#[tokio::test]
async fn admin_api_manages_principals_and_takes_effect_immediately() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let (a, b) = (prefix(), prefix());
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{a}*"), "access": "read_write" }]),
        )
        .await;
    let path = format!("/admin/principals/{}", tenant.name);

    // Tenants cannot use the admin API.
    assert_forbidden(api.get("/admin/principals", &tenant).await, "tenant list").await;
    assert_forbidden(api.get(&path, &tenant).await, "tenant get").await;

    // Get never exposes the hash.
    let resp = api.get(&path, &server.admin).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let view: Value = resp.json().await.unwrap();
    assert_eq!(view["role"], "tenant");
    assert_eq!(view["rules"][0]["pattern"], format!("{a}*"));
    assert!(view.get("password_hash").is_none() && view.get("passwordHash").is_none());

    let listed: Vec<Value> = api
        .get("/admin/principals", &server.admin)
        .await
        .json()
        .await
        .unwrap();
    assert!(listed.iter().any(|p| p["name"] == tenant.name.as_str()));

    // Duplicate name → 409; malformed definitions → 422.
    let resp = api
        .send(
            Method::POST,
            "/admin/principals",
            &server.admin,
            Some(json!({ "name": tenant.name, "password": "x", "role": "tenant" })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    for bad in [
        json!({ "name": "a:b", "password": "x", "role": "tenant" }),
        json!({ "name": "ok", "password": "", "role": "tenant" }),
        json!({ "name": "ok", "password": "x", "role": "root" }),
        json!({ "name": "ok", "password": "x", "role": "tenant", "rules": [{ "pattern": "a*b", "access": "read" }] }),
        json!({ "name": "ok", "password": "x", "role": "tenant", "rules": [{ "pattern": "a", "access": "write" }] }),
    ] {
        let resp = api
            .send(
                Method::POST,
                "/admin/principals",
                &server.admin,
                Some(bad.clone()),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error_code"], 42210, "{bad}");
    }

    // Warm the credential cache, then change the rules: effective at once.
    let other = format!("{b}x-value");
    assert_forbidden(
        api.get(&format!("/subjects/{other}/versions"), &tenant)
            .await,
        "before rule change",
    )
    .await;
    let resp = api
        .send(
            Method::PATCH,
            &path,
            &server.admin,
            Some(json!({ "rules": [{ "pattern": format!("{b}*"), "access": "read_write" }] })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    api.register_ok(&tenant, &other, &common::unique_avro_schema())
        .await;

    // Rotate the password: the old one stops working at once.
    let rotated = Creds {
        name: tenant.name.clone(),
        password: "rotated-password".into(),
    };
    let resp = api
        .send(
            Method::PATCH,
            &path,
            &server.admin,
            Some(json!({ "password": rotated.password })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        api.get("/subjects", &tenant).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api.get("/subjects", &rotated).await.status(),
        StatusCode::OK
    );

    // Promote to admin, then delete.
    let resp = api
        .send(
            Method::PATCH,
            &path,
            &server.admin,
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        api.get("/admin/principals", &rotated).await.status(),
        StatusCode::OK
    );
    let resp = api.send(Method::DELETE, &path, &server.admin, None).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        api.get("/subjects", &rotated).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let resp = api.send(Method::DELETE, &path, &server.admin, None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], 40410);
}

// -- Observability --

#[tokio::test]
async fn rejections_are_counted_per_principal() {
    let server = common::spawn_auth_server().await;
    let api = Api::new(&server);
    let tenant = api
        .tenant(
            &server.admin,
            json!([{ "pattern": format!("{}*", prefix()), "access": "read" }]),
        )
        .await;

    assert_forbidden(api.get("/admin/principals", &tenant).await, "403").await;
    let wrong = Creds {
        name: tenant.name.clone(),
        password: "nope".into(),
    };
    assert_eq!(
        api.get("/subjects", &wrong).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let unknown = Creds::unique("ghost");
    assert_eq!(
        api.get("/subjects", &unknown).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let body = api
        .req(Method::GET, "/metrics", None)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let has = |status: &str, principal: &str| {
        body.lines().any(|l| {
            l.starts_with("kora_auth_rejections_total{")
                && l.contains(&format!("status=\"{status}\""))
                && l.contains(&format!("principal=\"{principal}\""))
        })
    };
    assert!(has("403", &tenant.name), "{body}");
    assert!(has("401", &tenant.name), "{body}");
    assert!(has("401", "unknown"), "{body}");
    assert!(
        !body.contains(&unknown.name),
        "attempted names must not leak into labels"
    );
}
