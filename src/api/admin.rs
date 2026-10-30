//! Principal management API (`/admin/principals`), admin-only.
//!
//! Passwords are hashed with Argon2id before they reach storage and are never
//! returned. Changes evict the principal from this replica's auth cache at
//! once; other replicas pick them up within `AUTH_CACHE_TTL_SECS`.

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthState;
use crate::auth::{self, Role, Rule};
use crate::error::KoraError;
use crate::storage::DynStorage;
use crate::storage::types::{NewPrincipal, PrincipalRecord, PrincipalUpdate};

// -- Types --

/// Request body for `POST /admin/principals`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePrincipalRequest {
    /// Login name (no `:`).
    pub name: String,
    /// Clear-text password, hashed before storage.
    pub password: String,
    /// `admin` or `tenant`.
    pub role: Role,
    /// Subject rules (tenants only; ignored for admins).
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Request body for `PATCH /admin/principals/{name}`; absent fields are kept.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePrincipalRequest {
    /// New password (rotation).
    pub password: Option<String>,
    /// New role.
    pub role: Option<Role>,
    /// Replacement rule list.
    pub rules: Option<Vec<Rule>>,
}

/// A principal as returned by the admin API (never includes the hash).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalView {
    name: String,
    role: Role,
    rules: Vec<Rule>,
    created_at: String,
    updated_at: String,
}

impl From<PrincipalRecord> for PrincipalView {
    fn from(r: PrincipalRecord) -> Self {
        Self {
            name: r.name,
            role: r.role,
            rules: r.rules,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

// -- Handlers --

/// List principals.
///
/// `GET /admin/principals`
///
/// # Errors
///
/// Returns `KoraError::BackendDataStore` (500) for database failures.
pub async fn list_principals(
    State(storage): State<DynStorage>,
) -> Result<impl IntoResponse, KoraError> {
    let all = storage.list_principals().await?;
    Ok(Json(
        all.into_iter().map(PrincipalView::from).collect::<Vec<_>>(),
    ))
}

/// Create a principal.
///
/// `POST /admin/principals` → `201 Created`
///
/// # Errors
///
/// Returns `KoraError::InvalidPrincipal` (42210) for a malformed body, name,
/// password or rule, and `KoraError::PrincipalAlreadyExists` (40902) when the
/// name is taken.
pub async fn create_principal(
    State(storage): State<DynStorage>,
    body: Result<Json<CreatePrincipalRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let Json(body) = body.map_err(|e| KoraError::InvalidPrincipal(e.body_text()))?;
    auth::validate_name(&body.name)?;
    auth::validate_password(&body.password)?;
    auth::validate_rules(&body.rules)?;

    let hash = auth::hash_password_blocking(body.password).await?;
    let created = storage
        .create_principal(&NewPrincipal {
            name: &body.name,
            password_hash: &hash,
            role: body.role,
            rules: &body.rules,
        })
        .await?
        .ok_or_else(|| KoraError::PrincipalAlreadyExists(body.name.clone()))?;
    tracing::info!(principal = %created.name, role = created.role.as_str(), "principal created");
    Ok((StatusCode::CREATED, Json(PrincipalView::from(created))))
}

/// Get a principal.
///
/// `GET /admin/principals/{name}`
///
/// # Errors
///
/// Returns `KoraError::PrincipalNotFound` (40410) if it does not exist.
pub async fn get_principal(
    State(storage): State<DynStorage>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, KoraError> {
    let found = storage
        .find_principal(&name)
        .await?
        .ok_or(KoraError::PrincipalNotFound(name))?;
    Ok(Json(PrincipalView::from(found)))
}

/// Rotate a principal's password, change its role or replace its rules.
///
/// `PATCH /admin/principals/{name}`
///
/// # Errors
///
/// Returns `KoraError::InvalidPrincipal` (42210) for a malformed body,
/// password or rule, and `KoraError::PrincipalNotFound` (40410) if the
/// principal does not exist.
pub async fn update_principal(
    State(storage): State<DynStorage>,
    State(auth_state): State<AuthState>,
    Path(name): Path<String>,
    body: Result<Json<UpdatePrincipalRequest>, JsonRejection>,
) -> Result<impl IntoResponse, KoraError> {
    let Json(body) = body.map_err(|e| KoraError::InvalidPrincipal(e.body_text()))?;
    if let Some(password) = &body.password {
        auth::validate_password(password)?;
    }
    if let Some(rules) = &body.rules {
        auth::validate_rules(rules)?;
    }
    let hash = match body.password {
        Some(password) => Some(auth::hash_password_blocking(password).await?),
        None => None,
    };
    let updated = storage
        .update_principal(
            &name,
            &PrincipalUpdate {
                password_hash: hash.as_deref(),
                role: body.role,
                rules: body.rules.as_deref(),
            },
        )
        .await?
        .ok_or_else(|| KoraError::PrincipalNotFound(name.clone()))?;
    if let Some(cache) = auth_state.cache() {
        cache.evict_principal(updated.id, &updated.name);
    }
    tracing::info!(principal = %updated.name, "principal updated");
    Ok(Json(PrincipalView::from(updated)))
}

/// Delete a principal.
///
/// `DELETE /admin/principals/{name}` → `204 No Content`
///
/// # Errors
///
/// Returns `KoraError::PrincipalNotFound` (40410) if it does not exist.
pub async fn delete_principal(
    State(storage): State<DynStorage>,
    State(auth_state): State<AuthState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, KoraError> {
    let id = storage
        .delete_principal(&name)
        .await?
        .ok_or_else(|| KoraError::PrincipalNotFound(name.clone()))?;
    if let Some(cache) = auth_state.cache() {
        cache.evict_principal(id, &name);
    }
    tracing::info!(principal = %name, "principal deleted");
    Ok(StatusCode::NO_CONTENT)
}
