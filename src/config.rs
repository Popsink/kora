//! Application configuration loaded via figment.

use figment::{
    Figment,
    providers::{Env, Serialized},
};
use serde::{Deserialize, Serialize};

use crate::api::compatibility::COMPATIBILITY_LEVELS;

// -- Types --

/// Which database engine backs the registry.
///
/// `PostgreSQL` is the default and fully-supported engine. Oracle is additive
/// and only available in binaries built with the `oracle` cargo feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DbBackend {
    /// `PostgreSQL` (default).
    #[default]
    Postgres,
    /// Oracle Database.
    Oracle,
}

impl DbBackend {
    /// Parse a `DB_BACKEND` value (case-insensitive). `postgresql` aliases
    /// `postgres`.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => Some(Self::Postgres),
            "oracle" => Some(Self::Oracle),
            _ => None,
        }
    }
}

/// Top-level configuration for the Kora server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KoraConfig {
    /// Which database engine to use. Resolved from `DB_BACKEND`, or inferred from
    /// the `database_url` scheme (`oracle://` → Oracle), defaulting to `Postgres`.
    #[serde(default)]
    pub db_backend: DbBackend,
    /// Database connection string. For `PostgreSQL`, a `postgres://` URL passed
    /// straight to `sqlx`. For Oracle, an optional `oracle://user:pass@host:port/service`
    /// URL parsed into the `db_*` components. If empty, composed from the
    /// `DB_HOST` / `DB_PORT` / `DB_USER` / `DB_PASSWORD` / `DB_NAME` components.
    #[serde(default)]
    pub database_url: String,
    /// Database host (used when `database_url` is empty).
    #[serde(default)]
    pub db_host: String,
    /// Database port. Defaults to 5432 (`PostgreSQL`) or 1521 (Oracle).
    #[serde(default = "default_db_port")]
    pub db_port: u16,
    /// Database user.
    #[serde(default)]
    pub db_user: String,
    /// Database password.
    #[serde(default)]
    pub db_password: String,
    /// Database name (`PostgreSQL`) or service name (Oracle).
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
            db_backend: DbBackend::default(),
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
    /// Recognized env vars: `DB_BACKEND`, `DATABASE_URL`, `DB_HOST`, `DB_PORT`,
    /// `DB_USER`, `DB_PASSWORD`, `DB_NAME`, `HOST`, `PORT`, `MAX_BODY_SIZE`,
    /// `DB_POOL_MAX`, `DEFAULT_COMPATIBILITY`.
    ///
    /// The backend is taken from `DB_BACKEND` if set, otherwise inferred from the
    /// `database_url` scheme (`oracle://` → Oracle), defaulting to `Postgres`.
    /// For `PostgreSQL`, an empty `DATABASE_URL` is composed from the `DB_*`
    /// components (user/password percent-encoded). For Oracle, an `oracle://` URL
    /// is parsed into the `DB_*` components; otherwise the components are used
    /// directly. A blank `DEFAULT_COMPATIBILITY` is treated as unset.
    ///
    /// # Errors
    ///
    /// Returns an error if values cannot be parsed, if `DB_BACKEND` is unknown,
    /// if `DEFAULT_COMPATIBILITY` is not a known level, if an `oracle://` URL is
    /// malformed, or if neither a connection URL nor a complete `DB_*` set
    /// (`DB_HOST`, `DB_USER`, `DB_NAME`) is provided.
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

        cfg.resolve_backend()?;
        cfg.resolve_default_compatibility()?;
        cfg.resolve_connection()?;

        Ok(cfg)
    }

    /// Resolve [`DbBackend`] from `DB_BACKEND` or the URL scheme.
    fn resolve_backend(&mut self) -> Result<(), Box<figment::Error>> {
        if let Some(raw) = std::env::var("DB_BACKEND")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
        {
            self.db_backend = DbBackend::parse(&raw).ok_or_else(|| {
                Box::new(figment::Error::from(format!(
                    "DB_BACKEND is invalid: {raw}; must be one of: postgres, oracle"
                )))
            })?;
        } else if self.database_url.starts_with("oracle:") {
            self.db_backend = DbBackend::Oracle;
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

    /// Normalise the connection details for the resolved backend.
    fn resolve_connection(&mut self) -> Result<(), Box<figment::Error>> {
        // When no DB_PORT was given, fall back to the engine's standard port.
        if std::env::var("DB_PORT").is_err() && self.db_backend == DbBackend::Oracle {
            self.db_port = default_oracle_port();
        }

        match self.db_backend {
            DbBackend::Postgres => self.resolve_postgres(),
            DbBackend::Oracle => self.resolve_oracle(),
        }
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

    /// Parse an `oracle://` URL into components, or validate components directly.
    /// The Oracle driver connects from these components (host/port/service +
    /// user/password), so no URL is assembled.
    fn resolve_oracle(&mut self) -> Result<(), Box<figment::Error>> {
        if self.database_url.starts_with("oracle:") {
            let url = self.database_url.clone();
            self.apply_oracle_url(&url)?;
        }
        self.require_components()
    }

    /// Decompose an `oracle://user:pass@host:port/service` URL into `db_*`.
    fn apply_oracle_url(&mut self, url: &str) -> Result<(), Box<figment::Error>> {
        let rest = url
            .strip_prefix("oracle://")
            .or_else(|| url.strip_prefix("oracle:"))
            .unwrap_or(url);

        let (authority, service) = rest.split_once('/').ok_or_else(|| {
            Box::new(figment::Error::from(
                "oracle URL must include a service name: oracle://user:pass@host:port/service"
                    .to_owned(),
            ))
        })?;

        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((ui, hp)) => (Some(ui), hp),
            None => (None, authority),
        };

        if let Some(ui) = userinfo {
            let (user, pass) = ui.split_once(':').unwrap_or((ui, ""));
            self.db_user = decode_component(user);
            self.db_password = decode_component(pass);
        }

        let (host, port) = hostport
            .rsplit_once(':')
            .map_or((hostport, None), |(h, p)| (h, Some(p)));
        if !host.is_empty() {
            host.clone_into(&mut self.db_host);
        }
        if let Some(p) = port {
            self.db_port = p.parse().map_err(|_| {
                Box::new(figment::Error::from(format!(
                    "oracle URL has an invalid port: {p}"
                )))
            })?;
        }
        service.clone_into(&mut self.db_name);
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

/// Percent-decode a URL component, falling back to the raw value on error.
fn decode_component(s: &str) -> String {
    urlencoding::decode(s).map_or_else(|_| s.to_owned(), std::borrow::Cow::into_owned)
}

fn default_host() -> String {
    "0.0.0.0".to_owned()
}

fn default_port() -> u16 {
    8080
}

fn default_db_port() -> u16 {
    5432
}

fn default_oracle_port() -> u16 {
    1521
}

fn default_max_body_size() -> usize {
    16 * 1_024 * 1_024
}

fn default_db_pool_max() -> u32 {
    20
}
