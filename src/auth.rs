//! Principals, subject-scoped access rules and credential handling.
//!
//! Authentication is HTTP basic auth against principals stored in the
//! database (Argon2id password hashes). A principal is either an `admin`
//! (unrestricted) or a `tenant`, whose access is limited to the subjects
//! matched by its [`Rule`]s. The HTTP wiring (middleware, per-route policy)
//! lives in `api::auth`; this module holds the transport-agnostic pieces.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::KoraError;

// -- Types --

/// Principal role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Unrestricted: every subject, global `/config` and `/mode`, principal management.
    Admin,
    /// Restricted to the subjects matched by its rules.
    Tenant,
}

/// Access level granted by a [`Rule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Read the subject (versions, schemas, config, mode, lookups).
    Read,
    /// Read, register, delete and change the subject's compatibility config.
    ReadWrite,
}

/// A subject rule held by a tenant principal.
///
/// `pattern` is either an exact subject name or a prefix ending in `*`
/// (`d1a2b3c4d5.*`); a lone `*` matches every subject. `*` is only allowed as
/// the last character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Exact subject name, or prefix followed by `*`.
    pub pattern: String,
    /// Access granted on the matched subjects.
    pub access: Access,
}

/// The subjects a principal may read, in a form storage backends can push
/// down into their queries: a subject is in scope if it equals one of `names`
/// or starts with one of `prefixes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubjectScope {
    /// Exact subject names.
    pub names: Vec<String>,
    /// Subject name prefixes (an empty prefix matches everything).
    pub prefixes: Vec<String>,
}

/// An authenticated principal with its rules resolved.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Database ID (stable for the principal's lifetime; used as cache key).
    pub id: i64,
    /// Login name.
    pub name: String,
    /// Role.
    pub role: Role,
    /// Subject rules (ignored for admins).
    pub rules: Vec<Rule>,
    scope: SubjectScope,
}

/// The caller of a request, as seen by handlers.
#[derive(Debug, Clone)]
pub enum Caller {
    /// Authentication is disabled: everything is allowed.
    Unrestricted,
    /// An authenticated principal.
    Principal(Arc<Principal>),
}

// -- Impls --

impl Role {
    /// Wire/database representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Tenant => "tenant",
        }
    }

    /// Parse the database representation.
    ///
    /// # Errors
    ///
    /// Returns `KoraError::BackendDataStore` for an unknown role.
    pub fn parse(s: &str) -> Result<Self, KoraError> {
        match s {
            "admin" => Ok(Self::Admin),
            "tenant" => Ok(Self::Tenant),
            other => Err(KoraError::BackendDataStore(format!(
                "unknown principal role '{other}'"
            ))),
        }
    }
}

impl Rule {
    /// The prefix this rule matches, or `None` for an exact-name rule.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.pattern.strip_suffix('*')
    }

    /// Whether this rule covers `subject`.
    #[must_use]
    pub fn matches(&self, subject: &str) -> bool {
        match self.prefix() {
            Some(prefix) => subject.starts_with(prefix),
            None => subject == self.pattern,
        }
    }

    /// Check the pattern is well-formed.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the pattern is empty or has a `*`
    /// anywhere but at the end.
    pub fn validate(&self) -> Result<(), String> {
        if self.pattern.is_empty() {
            return Err("rule pattern must not be empty".into());
        }
        let body = self.prefix().unwrap_or(&self.pattern);
        if body.contains('*') {
            return Err(format!(
                "rule pattern '{}': '*' is only allowed as the last character",
                self.pattern
            ));
        }
        Ok(())
    }
}

impl SubjectScope {
    fn from_rules(rules: &[Rule]) -> Self {
        let mut scope = Self::default();
        for rule in rules {
            match rule.prefix() {
                Some(prefix) => scope.prefixes.push(prefix.to_owned()),
                None => scope.names.push(rule.pattern.clone()),
            }
        }
        scope
    }
}

impl Principal {
    /// Build a principal, resolving its read scope from its rules.
    #[must_use]
    pub fn new(id: i64, name: String, role: Role, rules: Vec<Rule>) -> Self {
        let scope = SubjectScope::from_rules(&rules);
        Self {
            id,
            name,
            role,
            rules,
            scope,
        }
    }

    /// Whether the principal is an admin.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    /// Whether the principal may read `subject`.
    #[must_use]
    pub fn can_read(&self, subject: &str) -> bool {
        self.is_admin() || self.rules.iter().any(|r| r.matches(subject))
    }

    /// Whether the principal may write `subject`.
    #[must_use]
    pub fn can_write(&self, subject: &str) -> bool {
        self.is_admin()
            || self
                .rules
                .iter()
                .any(|r| r.access == Access::ReadWrite && r.matches(subject))
    }
}

impl Caller {
    /// Read scope to push down into listing queries; `None` means unrestricted.
    #[must_use]
    pub fn read_scope(&self) -> Option<&SubjectScope> {
        match self {
            Self::Principal(p) if !p.is_admin() => Some(&p.scope),
            _ => None,
        }
    }

    /// Principal name for logs and metrics, if authenticated.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Unrestricted => None,
            Self::Principal(p) => Some(&p.name),
        }
    }

    /// Require read access on `subject`.
    ///
    /// # Errors
    ///
    /// Returns `KoraError::Forbidden` (40301) when the principal has no rule
    /// covering the subject.
    pub fn require_read(&self, subject: &str) -> Result<(), KoraError> {
        match self {
            Self::Principal(p) if !p.can_read(subject) => {
                Err(forbidden(&p.name, "read", Some(subject)))
            }
            _ => Ok(()),
        }
    }

    /// Require write access on `subject`.
    ///
    /// # Errors
    ///
    /// Returns `KoraError::Forbidden` (40301) when no `read_write` rule covers
    /// the subject.
    pub fn require_write(&self, subject: &str) -> Result<(), KoraError> {
        match self {
            Self::Principal(p) if !p.can_write(subject) => {
                Err(forbidden(&p.name, "write", Some(subject)))
            }
            _ => Ok(()),
        }
    }

    /// Require the admin role.
    ///
    /// # Errors
    ///
    /// Returns `KoraError::Forbidden` (40301) for tenant principals.
    pub fn require_admin(&self) -> Result<(), KoraError> {
        match self {
            Self::Principal(p) if !p.is_admin() => Err(forbidden(&p.name, "administer", None)),
            _ => Ok(()),
        }
    }
}

fn forbidden(principal: &str, operation: &str, subject: Option<&str>) -> KoraError {
    KoraError::Forbidden(match subject {
        Some(s) => {
            format!("Principal '{principal}' is not authorized to {operation} subject '{s}'")
        }
        None => format!("Principal '{principal}' is not authorized to {operation} the registry"),
    })
}

// -- Validation --

/// Maximum principal name length.
const MAX_PRINCIPAL_NAME_LENGTH: usize = 255;

/// Validate a principal name: non-empty, bounded, no `:` (the basic-auth
/// separator) and no control characters.
///
/// # Errors
///
/// Returns `KoraError::InvalidPrincipal` (42210) describing the problem.
pub fn validate_name(name: &str) -> Result<(), KoraError> {
    if name.is_empty() || name.len() > MAX_PRINCIPAL_NAME_LENGTH {
        return Err(KoraError::InvalidPrincipal(format!(
            "name must be 1-{MAX_PRINCIPAL_NAME_LENGTH} bytes"
        )));
    }
    if name.contains(':') || name.chars().any(char::is_control) {
        return Err(KoraError::InvalidPrincipal(
            "name must not contain ':' or control characters".into(),
        ));
    }
    Ok(())
}

/// Validate a password (non-empty).
///
/// # Errors
///
/// Returns `KoraError::InvalidPrincipal` (42210) for an empty password.
pub fn validate_password(password: &str) -> Result<(), KoraError> {
    if password.is_empty() {
        return Err(KoraError::InvalidPrincipal(
            "password must not be empty".into(),
        ));
    }
    Ok(())
}

/// Validate every rule of a rule list.
///
/// # Errors
///
/// Returns `KoraError::InvalidPrincipal` (42210) for the first malformed rule.
pub fn validate_rules(rules: &[Rule]) -> Result<(), KoraError> {
    rules
        .iter()
        .try_for_each(Rule::validate)
        .map_err(KoraError::InvalidPrincipal)
}

// -- Password hashing --

/// Hash a password into an Argon2id PHC string (CPU-heavy: run off the async
/// executor, e.g. via `spawn_blocking`).
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` if hashing fails.
pub fn hash_password(password: &str) -> Result<String, KoraError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| KoraError::BackendDataStore(format!("password hashing failed: {e}")))
}

/// Verify a password against an Argon2 PHC string (CPU-heavy, see
/// [`hash_password`]). A malformed hash never verifies.
#[must_use]
pub fn verify_password(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// [`hash_password`] on the blocking thread pool.
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` if hashing fails or the task panics.
pub async fn hash_password_blocking(password: String) -> Result<String, KoraError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| KoraError::BackendDataStore(format!("password hashing task failed: {e}")))?
}

/// [`verify_password`] on the blocking thread pool.
pub async fn verify_password_blocking(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &phc))
        .await
        .unwrap_or(false)
}

// -- Cache --

/// Upper bound on entries per cache map; on overflow the map is cleared.
const MAX_CACHE_ENTRIES: usize = 100_000;

struct CachedCredential {
    digest: [u8; 32],
    principal: Arc<Principal>,
    expires: Instant,
}

/// In-process cache for verified credentials and per-principal schema-ID
/// read decisions.
///
/// Argon2 verification is deliberately slow and basic auth resends the
/// password on every request, so a verified `(name, SHA-256(password))` pair
/// is remembered for `ttl`. Only positive decisions are cached; rotating,
/// editing or deleting a principal through the admin API evicts it on this
/// replica immediately, and on other replicas after at most `ttl`. A zero
/// `ttl` disables caching.
pub struct AuthCache {
    ttl: Duration,
    credentials: Mutex<HashMap<String, CachedCredential>>,
    readable_ids: Mutex<HashMap<(i64, i64), Instant>>,
}

impl AuthCache {
    /// Create an empty cache whose entries live for `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            credentials: Mutex::new(HashMap::new()),
            readable_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Return the cached principal if `name` / `password` were verified within
    /// the TTL.
    #[must_use]
    pub fn credential(&self, name: &str, password: &str) -> Option<Arc<Principal>> {
        let digest = sha256(password);
        let map = self
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.get(name)
            .filter(|c| c.digest == digest && c.expires > Instant::now())
            .map(|c| Arc::clone(&c.principal))
    }

    /// Remember a successful verification.
    pub fn store_credential(&self, password: &str, principal: Arc<Principal>) {
        if self.ttl.is_zero() {
            return;
        }
        let mut map = self
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_CACHE_ENTRIES {
            map.clear();
        }
        map.insert(
            principal.name.clone(),
            CachedCredential {
                digest: sha256(password),
                principal,
                expires: Instant::now() + self.ttl,
            },
        );
    }

    /// Whether principal `principal_id` was recently found able to read
    /// schema `schema_id`.
    #[must_use]
    pub fn schema_readable(&self, principal_id: i64, schema_id: i64) -> bool {
        let map = self
            .readable_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.get(&(principal_id, schema_id))
            .is_some_and(|expires| *expires > Instant::now())
    }

    /// Remember that `principal_id` can read `schema_id`.
    pub fn store_schema_readable(&self, principal_id: i64, schema_id: i64) {
        if self.ttl.is_zero() {
            return;
        }
        let mut map = self
            .readable_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_CACHE_ENTRIES {
            map.clear();
        }
        map.insert((principal_id, schema_id), Instant::now() + self.ttl);
    }

    /// Drop everything cached for a principal (after a rotation, rule change
    /// or deletion).
    pub fn evict_principal(&self, principal_id: i64, name: &str) {
        self.credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        self.readable_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(pid, _), _| *pid != principal_id);
    }
}

fn sha256(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, access: Access) -> Rule {
        Rule {
            pattern: pattern.into(),
            access,
        }
    }

    #[test]
    fn prefix_and_exact_rules_match() {
        let p = Principal::new(
            1,
            "a".into(),
            Role::Tenant,
            vec![
                rule("a.*", Access::ReadWrite),
                rule("legacy-value", Access::Read),
            ],
        );
        assert!(p.can_write("a.x-value"));
        assert!(p.can_read("legacy-value"));
        assert!(!p.can_write("legacy-value"));
        assert!(!p.can_read("legacy-value-2"));
        assert!(!p.can_read("b.x-value"));
        assert_eq!(p.scope.names, vec!["legacy-value".to_owned()]);
        assert_eq!(p.scope.prefixes, vec!["a.".to_owned()]);
    }

    #[test]
    fn star_matches_everything() {
        let p = Principal::new(1, "r".into(), Role::Tenant, vec![rule("*", Access::Read)]);
        assert!(p.can_read("anything"));
        assert!(!p.can_write("anything"));
    }

    #[test]
    fn rule_validation() {
        assert!(rule("a.*", Access::Read).validate().is_ok());
        assert!(rule("*", Access::Read).validate().is_ok());
        assert!(rule("", Access::Read).validate().is_err());
        assert!(rule("a*b", Access::Read).validate().is_err());
        assert!(rule("a**", Access::Read).validate().is_err());
    }

    #[test]
    fn password_round_trip() {
        let hash = hash_password("s3cret").unwrap();
        assert!(verify_password("s3cret", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("s3cret", "not-a-phc-string"));
    }

    #[test]
    fn credential_cache_requires_same_password() {
        let cache = AuthCache::new(Duration::from_mins(1));
        let p = Arc::new(Principal::new(7, "a".into(), Role::Tenant, vec![]));
        cache.store_credential("pw", Arc::clone(&p));
        assert!(cache.credential("a", "pw").is_some());
        assert!(cache.credential("a", "other").is_none());
        cache.store_schema_readable(7, 42);
        assert!(cache.schema_readable(7, 42));
        cache.evict_principal(7, "a");
        assert!(cache.credential("a", "pw").is_none());
        assert!(!cache.schema_readable(7, 42));
    }

    #[test]
    fn zero_ttl_disables_cache() {
        let cache = AuthCache::new(Duration::ZERO);
        let p = Arc::new(Principal::new(7, "a".into(), Role::Tenant, vec![]));
        cache.store_credential("pw", p);
        assert!(cache.credential("a", "pw").is_none());
    }
}
