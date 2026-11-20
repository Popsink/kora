//! HTTP basic authentication and the per-route subject ACL.
//!
//! [`enforce`] wraps every API route except `/health` (and the operational
//! `/metrics`): it authenticates the caller, applies the route-level rule from
//! [`requirement`], and hands the resulting [`Caller`] to handlers, which
//! filter listings and schema-ID lookups by the caller's read scope.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::{
    extract::{FromRef, FromRequestParts, MatchedPath, RawPathParams, Request, State},
    http::{HeaderMap, HeaderValue, Method, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

use crate::auth::{self, AuthCache, Caller, Principal};
use crate::error::KoraError;
use crate::storage::DynStorage;

// -- State --

/// Authentication state shared through the router; `None` when
/// `AUTH_ENABLED=false`.
#[derive(Clone)]
pub struct AuthState(Option<Arc<Authenticator>>);

/// Verifies credentials against stored principals, with caching.
pub struct Authenticator {
    storage: DynStorage,
    cache: AuthCache,
}

impl AuthState {
    /// Authentication disabled: every request is unrestricted.
    #[must_use]
    pub fn disabled() -> Self {
        Self(None)
    }

    /// Authentication enabled against `storage`, caching positive decisions
    /// for `cache_ttl`.
    #[must_use]
    pub fn enabled(storage: DynStorage, cache_ttl: Duration) -> Self {
        Self(Some(Arc::new(Authenticator {
            storage,
            cache: AuthCache::new(cache_ttl),
        })))
    }

    /// Whether authentication is enforced.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// The credential/decision cache, when authentication is enabled.
    #[must_use]
    pub fn cache(&self) -> Option<&AuthCache> {
        self.0.as_deref().map(|a| &a.cache)
    }

    /// Require `caller` to be able to read schema `id`, i.e. the ID is
    /// registered under at least one subject in its read scope. Unrestricted
    /// callers always pass; positive decisions are cached per principal.
    ///
    /// # Errors
    ///
    /// Returns `KoraError::SchemaNotFound` (40403) when the ID is not readable
    /// (indistinguishable from a missing ID), or a database error.
    pub async fn ensure_schema_readable(
        &self,
        storage: &DynStorage,
        caller: &Caller,
        id: i64,
    ) -> Result<(), KoraError> {
        let (Some(scope), Caller::Principal(principal)) = (caller.read_scope(), caller) else {
            return Ok(());
        };
        let cache = self.cache();
        if cache.is_some_and(|c| c.schema_readable(principal.id, id)) {
            return Ok(());
        }
        if !storage.schema_readable(id, scope).await? {
            return Err(KoraError::SchemaNotFound);
        }
        if let Some(c) = cache {
            c.store_schema_readable(principal.id, id);
        }
        Ok(())
    }
}

// -- Caller extraction --

impl<S> FromRequestParts<S> for Caller
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = KoraError;

    /// The caller set by [`enforce`]. Fails closed: with authentication enabled
    /// a missing caller (a route not wrapped by `enforce`) is rejected.
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(caller) = parts.extensions.get::<Self>() {
            return Ok(caller.clone());
        }
        if AuthState::from_ref(state).is_enabled() {
            Err(KoraError::Unauthenticated)
        } else {
            Ok(Self::Unrestricted)
        }
    }
}

// -- Middleware --

/// Authenticate the request and enforce the route-level ACL.
pub async fn enforce(State(auth): State<AuthState>, req: Request, next: Next) -> Response {
    let Some(authenticator) = auth.0 else {
        return next.run(req).await;
    };
    let (mut parts, body) = req.into_parts();

    let principal = match authenticator.authenticate(&parts.headers).await {
        Ok(p) => p,
        Err(failure) => {
            let mut resp = unauthorized();
            resp.extensions_mut().insert(failure.label());
            return resp;
        }
    };
    let label = PrincipalLabel(principal.name.clone());
    let caller = Caller::Principal(principal);

    let route = parts
        .extensions
        .get::<MatchedPath>()
        .map(|mp| mp.as_str().to_owned());
    let params = RawPathParams::from_request_parts(&mut parts, &())
        .await
        .ok();
    let subject = params
        .as_ref()
        .and_then(|p| p.iter().find(|(k, _)| *k == "subject").map(|(_, v)| v));

    if let Some(route) = route.as_deref()
        && let Err(e) = check(&caller, &parts.method, route, subject)
    {
        let mut resp = e.into_response();
        resp.extensions_mut().insert(label);
        return resp;
    }

    parts.extensions.insert(caller);
    let mut resp = next.run(Request::from_parts(parts, body)).await;
    resp.extensions_mut().insert(label);
    resp
}

/// Principal name attached to responses for request logs and the
/// `kora_auth_rejections_total` metric. On a 401 it is the attempted name only
/// when that principal exists (`unknown` otherwise, `anonymous` without
/// credentials), so arbitrary usernames cannot blow up metric cardinality.
#[derive(Debug, Clone)]
pub struct PrincipalLabel(pub String);

/// 401 with a basic-auth challenge so standard clients retry with credentials.
fn unauthorized() -> Response {
    let mut resp = KoraError::Unauthenticated.into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"kora\", charset=\"UTF-8\""),
    );
    resp
}

// -- Authentication --

/// Why a request failed authentication.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthFailure {
    /// No (or malformed) `Authorization: Basic` header.
    MissingCredentials,
    /// No principal of that name.
    UnknownPrincipal,
    /// The principal exists but the password is wrong.
    WrongPassword(String),
}

impl AuthFailure {
    fn label(self) -> PrincipalLabel {
        PrincipalLabel(match self {
            Self::MissingCredentials => "anonymous".to_owned(),
            Self::UnknownPrincipal => "unknown".to_owned(),
            Self::WrongPassword(name) => name,
        })
    }
}

/// Hash verified against when the principal does not exist, so unknown and
/// known names take the same time to reject.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| auth::hash_password("kora-dummy-password").unwrap_or_default());

impl Authenticator {
    async fn authenticate(&self, headers: &HeaderMap) -> Result<Arc<Principal>, AuthFailure> {
        let (name, password) = basic_credentials(headers).ok_or(AuthFailure::MissingCredentials)?;
        if let Some(principal) = self.cache.credential(&name, &password) {
            return Ok(principal);
        }
        let record = self.storage.find_principal(&name).await.map_err(|e| {
            tracing::error!(error = %e, "principal lookup failed");
            AuthFailure::UnknownPrincipal
        })?;
        let Some(record) = record else {
            let _ = tokio::task::spawn_blocking(move || {
                auth::verify_password(&password, LazyLock::force(&DUMMY_HASH))
            })
            .await;
            return Err(AuthFailure::UnknownPrincipal);
        };
        if !auth::verify_password_blocking(password.clone(), record.password_hash.clone()).await {
            return Err(AuthFailure::WrongPassword(name));
        }
        let principal = Arc::new(record.to_principal());
        self.cache
            .store_credential(&password, Arc::clone(&principal));
        Ok(principal)
    }
}

/// Decode `Authorization: Basic base64(name:password)`.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(BASE64.decode(encoded.trim()).ok()?).ok()?;
    let (name, password) = decoded.split_once(':')?;
    Some((name.to_owned(), password.to_owned()))
}

// -- Route policy --

/// What a route needs beyond authentication (the issue #91 enforcement table).
#[derive(Debug, PartialEq, Eq)]
enum Requirement<'a> {
    /// Any authenticated principal (handlers filter by read scope).
    Authenticated,
    /// Admin role only.
    Admin,
    /// `read` on the subject.
    Read(&'a str),
    /// `read_write` on the subject.
    Write(&'a str),
}

/// Map a matched route to its requirement.
///
/// - `/admin/**`: admin.
/// - `/mode/{subject}`: `GET` needs read; writes are admin-only.
/// - Any other route with a `{subject}`: `GET` needs read, as do the
///   non-mutating `POST /subjects/{subject}` (lookup) and
///   `POST /compatibility/subjects/{subject}/**` (compatibility test); other
///   methods need `read_write`.
/// - Global `/config` and `/mode`: `GET` for everyone, writes admin-only.
/// - Everything else (listings, `/schemas/ids/**`, `/`): authenticated, with
///   the handler applying the read scope.
fn requirement<'a>(method: &Method, route: &str, subject: Option<&'a str>) -> Requirement<'a> {
    let is_read = method == Method::GET || method == Method::HEAD;
    if route == "/admin" || route.starts_with("/admin/") {
        return Requirement::Admin;
    }
    if let Some(subject) = subject {
        if route.starts_with("/mode/") {
            return if is_read {
                Requirement::Read(subject)
            } else {
                Requirement::Admin
            };
        }
        let non_mutating_post = method == Method::POST
            && (route == "/subjects/{subject}" || route.starts_with("/compatibility/"));
        return if is_read || non_mutating_post {
            Requirement::Read(subject)
        } else {
            Requirement::Write(subject)
        };
    }
    if (route == "/config" || route == "/mode") && !is_read {
        return Requirement::Admin;
    }
    Requirement::Authenticated
}

fn check(
    caller: &Caller,
    method: &Method,
    route: &str,
    subject: Option<&str>,
) -> Result<(), KoraError> {
    match requirement(method, route, subject) {
        Requirement::Authenticated => Ok(()),
        Requirement::Admin => caller.require_admin(),
        Requirement::Read(s) => caller.require_read(s),
        Requirement::Write(s) => caller.require_write(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_table() {
        use Requirement::{Admin, Authenticated, Read, Write};
        let s = Some("a.x");
        let cases = [
            (Method::GET, "/subjects/{subject}/versions", s, Read("a.x")),
            (
                Method::POST,
                "/subjects/{subject}/versions",
                s,
                Write("a.x"),
            ),
            (
                Method::GET,
                "/subjects/{subject}/versions/{version}",
                s,
                Read("a.x"),
            ),
            (
                Method::DELETE,
                "/subjects/{subject}/versions/{version}",
                s,
                Write("a.x"),
            ),
            (Method::POST, "/subjects/{subject}", s, Read("a.x")),
            (Method::DELETE, "/subjects/{subject}", s, Write("a.x")),
            (
                Method::POST,
                "/compatibility/subjects/{subject}/versions",
                s,
                Read("a.x"),
            ),
            (Method::GET, "/config/{subject}", s, Read("a.x")),
            (Method::PUT, "/config/{subject}", s, Write("a.x")),
            (Method::DELETE, "/config/{subject}", s, Write("a.x")),
            (Method::GET, "/mode/{subject}", s, Read("a.x")),
            (Method::PUT, "/mode/{subject}", s, Admin),
            (Method::DELETE, "/mode/{subject}", s, Admin),
            (Method::GET, "/config", None, Authenticated),
            (Method::PUT, "/config", None, Admin),
            (Method::DELETE, "/mode", None, Admin),
            (Method::GET, "/subjects", None, Authenticated),
            (Method::GET, "/schemas/ids/{id}", None, Authenticated),
            (Method::GET, "/admin/principals", None, Admin),
        ];
        for (method, route, subject, expected) in cases {
            assert_eq!(
                requirement(&method, route, subject),
                expected,
                "{method} {route}"
            );
        }
    }

    #[test]
    fn parses_basic_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {}", BASE64.encode("a:p:w"))).unwrap(),
        );
        assert_eq!(
            basic_credentials(&headers),
            Some(("a".into(), "p:w".into()))
        );
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        assert_eq!(basic_credentials(&headers), None);
    }
}
