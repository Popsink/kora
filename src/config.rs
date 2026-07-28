//! Application configuration loaded via figment.

use figment::{
    Figment,
    providers::{Env, Serialized},
};
use serde::{Deserialize, Serialize};

use crate::api::compatibility::COMPATIBILITY_LEVELS;

// -- Types --

/// Top-level configuration for the Kora server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KoraConfig {
    /// `PostgreSQL` connection string (`postgres://` URL passed straight to
    /// `sqlx`). If empty, composed from the `DB_HOST` / `DB_PORT` / `DB_USER` /
    /// `DB_PASSWORD` / `DB_NAME` components.
    #[serde(default)]
    pub database_url: String,
    /// Database host (used when `database_url` is empty).
    #[serde(default)]
    pub db_host: String,
    /// Database port. Defaults to 5432.
    #[serde(default = "default_db_port")]
    pub db_port: u16,
    /// Database user.
    #[serde(default)]
    pub db_user: String,
    /// Database password.
    #[serde(default)]
    pub db_password: String,
    /// Database name.
    #[serde(default)]
    pub db_name: String,
    /// Host address to bind the server to.
    #[serde(default = "default_host")]
    pub host: String,
    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Maximum request body size in bytes.
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
    /// Maximum number of database connections in the pool.
    #[serde(default = "default_db_pool_max")]
    pub db_pool_max: u32,
    /// Default global schema compatibility level. When set, it is reconciled into
    /// the global config row (`subject IS NULL`) on every startup — the declared
    /// default is the source of truth, overwriting any runtime `PUT`/`DELETE
    /// /config` change to the global level. When unset (or blank), the stored
    /// level is kept (built-in default `BACKWARD`). Validated on load against
    /// `COMPATIBILITY_LEVELS`.
    #[serde(default)]
    pub default_compatibility: Option<String>,
}

// -- Impls --

impl Default for KoraConfig {
    fn default() -> Self {
        Self {
            database_url: String::new(),
            db_host: String::new(),
            db_port: default_db_port(),
            db_user: String::new(),
            db_password: String::new(),
            db_name: String::new(),
            host: default_host(),
            port: default_port(),
            max_body_size: default_max_body_size(),
            db_pool_max: default_db_pool_max(),
            default_compatibility: None,
        }
    }
}

impl KoraConfig {
    /// Load configuration from defaults and environment variables.
    ///
    /// Recognized env vars: `DATABASE_URL`, `DB_HOST`, `DB_PORT`, `DB_USER`,
    /// `DB_PASSWORD`, `DB_NAME`, `HOST`, `PORT`, `MAX_BODY_SIZE`, `DB_POOL_MAX`,
    /// `DEFAULT_COMPATIBILITY`.
    ///
    /// An empty `DATABASE_URL` is composed from the `DB_*` components
    /// (user/password percent-encoded). A blank `DEFAULT_COMPATIBILITY` is
    /// treated as unset.
    ///
    /// # Errors
    ///
    /// Returns an error if values cannot be parsed, if `DEFAULT_COMPATIBILITY`
    /// is not a known level, if a removed Oracle selector is present
    /// (`DB_BACKEND` set to anything but `postgres`, or an `oracle://` URL), or
    /// if neither a connection URL nor a complete `DB_*` set (`DB_HOST`,
    /// `DB_USER`, `DB_NAME`) is provided.
    pub fn load() -> Result<Self, Box<figment::Error>> {
        let mut cfg: Self = Figment::from(Serialized::defaults(Self::default()))
            .merge(Env::raw().only(&[
                "DATABASE_URL",
                "DB_HOST",
                "DB_PORT",
                "DB_USER",
                "DB_PASSWORD",
                "DB_NAME",
                "HOST",
                "PORT",
                "MAX_BODY_SIZE",
                "DB_POOL_MAX",
            ]))
            .extract()
            .map_err(Box::new)?;

        cfg.reject_removed_backend()?;
        cfg.resolve_default_compatibility()?;
        cfg.resolve_postgres()?;

        Ok(cfg)
    }

    /// Fail fast on the removed Oracle backend selectors instead of silently
    /// starting against `PostgreSQL`. `DB_BACKEND=postgres` (and `postgresql`)
    /// is tolerated as a harmless legacy no-op so existing deployments keep
    /// working.
    fn reject_removed_backend(&self) -> Result<(), Box<figment::Error>> {
        if let Ok(raw) = std::env::var("DB_BACKEND") {
            let v = raw.trim();
            if !v.is_empty()
                && !v.eq_ignore_ascii_case("postgres")
                && !v.eq_ignore_ascii_case("postgresql")
            {
                return Err(Box::new(figment::Error::from(format!(
                    "DB_BACKEND is set to '{v}', but PostgreSQL is the only supported \
                     backend (Oracle support was removed in favour of PostgreSQL); unset \
                     DB_BACKEND and provide a postgres:// DATABASE_URL"
                ))));
            }
        }
        if self.database_url.starts_with("oracle:") {
            return Err(Box::new(figment::Error::from(
                "DATABASE_URL uses the removed oracle:// scheme (Oracle support was \
                 removed); provide a postgres:// DSN instead"
                    .to_owned(),
            )));
        }
        Ok(())
    }

    /// Read and validate `DEFAULT_COMPATIBILITY` directly (bypassing figment's
    /// bool/numeric coercion so a typo yields an actionable error).
    fn resolve_default_compatibility(&mut self) -> Result<(), Box<figment::Error>> {
        self.default_compatibility = std::env::var("DEFAULT_COMPATIBILITY")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if let Some(level) = &self.default_compatibility
            && !COMPATIBILITY_LEVELS.contains(&level.as_str())
        {
            return Err(Box::new(figment::Error::from(format!(
                "DEFAULT_COMPATIBILITY is invalid: {level}; must be one of: {}",
                COMPATIBILITY_LEVELS.join(", ")
            ))));
        }
        Ok(())
    }

    /// Compose a `postgres://` URL from components when none was provided.
    fn resolve_postgres(&mut self) -> Result<(), Box<figment::Error>> {
        if self.database_url.is_empty() {
            self.require_components()?;
            self.database_url = format!(
                "postgres://{}:{}@{}:{}/{}",
                urlencoding::encode(&self.db_user),
                urlencoding::encode(&self.db_password),
                self.db_host,
                self.db_port,
                self.db_name,
            );
        }
        Ok(())
    }

    /// Require the minimal component set when no usable URL was provided.
    fn require_components(&self) -> Result<(), Box<figment::Error>> {
        let mut missing = Vec::new();
        if self.db_host.is_empty() {
            missing.push("DB_HOST");
        }
        if self.db_user.is_empty() {
            missing.push("DB_USER");
        }
        if self.db_name.is_empty() {
            missing.push("DB_NAME");
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(Box::new(figment::Error::from(format!(
            "DATABASE_URL is unset; cannot compose from components — missing: {}",
            missing.join(", ")
        ))))
    }
}

// -- Helpers --

fn default_host() -> String {
    "0.0.0.0".to_owned()
}

fn default_port() -> u16 {
    8080
}

fn default_db_port() -> u16 {
    5432
}

fn default_max_body_size() -> usize {
    16 * 1_024 * 1_024
}

fn default_db_pool_max() -> u32 {
    20
}
