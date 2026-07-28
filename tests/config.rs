//! Tests for application configuration.
#![allow(clippy::result_large_err)] // Jail::expect_with's closure must return Result<_, figment::Error>; we can't box.

use figment::{Figment, Jail, providers::Serialized};
use kora::config::KoraConfig;

#[test]
fn config_defaults_applied() {
    let cfg: KoraConfig = Figment::from(Serialized::defaults(KoraConfig::default()))
        .extract()
        .expect("defaults should parse");

    assert_eq!(cfg.host, "0.0.0.0");
    assert_eq!(cfg.port, 8080);
    assert!(cfg.database_url.is_empty());
    assert_eq!(cfg.db_pool_max, 20);
    assert_eq!(cfg.default_compatibility, None);
}

#[test]
fn config_env_overrides_defaults() {
    let cfg: KoraConfig = Figment::from(Serialized::defaults(KoraConfig::default()))
        .merge(("port", 9090_u16))
        .merge(("host", "127.0.0.1"))
        .merge(("database_url", "postgres://test:test@localhost/test"))
        .extract()
        .expect("overrides should parse");

    assert_eq!(cfg.port, 9090);
    assert_eq!(cfg.host, "127.0.0.1");
    assert_eq!(cfg.database_url, "postgres://test:test@localhost/test");
}

#[test]
fn load_uses_database_url_env_when_set() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DB_HOST", "should-be-ignored");

        let cfg = KoraConfig::load().expect("load should succeed");

        assert_eq!(cfg.database_url, "postgres://from-env/db");
        Ok(())
    });
}

#[test]
fn load_composes_database_url_from_components() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "");
        jail.set_env("DB_HOST", "pg.local");
        jail.set_env("DB_PORT", "6543");
        jail.set_env("DB_USER", "ko@ra");
        jail.set_env("DB_PASSWORD", "p@ss/word");
        jail.set_env("DB_NAME", "kora");

        let cfg = KoraConfig::load().expect("load should succeed");

        assert_eq!(
            cfg.database_url,
            "postgres://ko%40ra:p%40ss%2Fword@pg.local:6543/kora"
        );
        Ok(())
    });
}

#[test]
fn load_errors_when_neither_url_nor_components_provided() {
    Jail::expect_with(|jail| {
        // Hermetic: clear any ambient DB_* so the "no URL, no components" error
        // path is what is exercised.
        jail.set_env("DATABASE_URL", "");
        jail.set_env("DB_HOST", "");
        jail.set_env("DB_USER", "");
        jail.set_env("DB_NAME", "");
        let err = KoraConfig::load().expect_err("load should fail");
        let msg = err.to_string();

        assert!(msg.contains("DATABASE_URL"), "{msg}");
        assert!(msg.contains("DB_HOST"), "{msg}");
        Ok(())
    });
}

#[test]
fn load_rejects_oracle_url_with_actionable_error() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "oracle://kora:secret@db-host:1521/FREEPDB1");

        let err = KoraConfig::load().expect_err("oracle:// URL should fail load");
        let msg = err.to_string();

        assert!(msg.contains("oracle://"), "{msg}");
        assert!(msg.contains("postgres://"), "{msg}");
        Ok(())
    });
}

#[test]
fn load_rejects_oracle_db_backend_with_actionable_error() {
    Jail::expect_with(|jail| {
        jail.set_env("DB_BACKEND", "oracle");
        jail.set_env("DATABASE_URL", "postgres://from-env/db");

        let err = KoraConfig::load().expect_err("DB_BACKEND=oracle should fail load");
        let msg = err.to_string();

        assert!(msg.contains("DB_BACKEND"), "{msg}");
        assert!(msg.contains("PostgreSQL"), "{msg}");
        Ok(())
    });
}

#[test]
fn load_tolerates_legacy_postgres_db_backend() {
    Jail::expect_with(|jail| {
        jail.set_env("DB_BACKEND", "postgres");
        jail.set_env("DATABASE_URL", "postgres://from-env/db");

        let cfg = KoraConfig::load().expect("DB_BACKEND=postgres should stay a no-op");

        assert_eq!(cfg.database_url, "postgres://from-env/db");
        Ok(())
    });
}

#[test]
fn load_accepts_valid_default_compatibility() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "FULL");

        let cfg = KoraConfig::load().expect("valid level should load");

        assert_eq!(cfg.default_compatibility.as_deref(), Some("FULL"));
        Ok(())
    });
}

#[test]
fn load_rejects_invalid_default_compatibility() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "SIDEWAYS");

        let err = KoraConfig::load().expect_err("invalid level should fail load");
        let msg = err.to_string();

        assert!(msg.contains("DEFAULT_COMPATIBILITY"), "{msg}");
        assert!(msg.contains("SIDEWAYS"), "{msg}");
        Ok(())
    });
}

#[test]
fn load_treats_empty_default_compatibility_as_unset() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "");

        let cfg = KoraConfig::load().expect("empty level should be treated as unset");

        assert_eq!(cfg.default_compatibility, None);
        Ok(())
    });
}

#[test]
fn load_treats_whitespace_default_compatibility_as_unset() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "   ");

        let cfg = KoraConfig::load().expect("whitespace-only should be treated as unset");

        assert_eq!(cfg.default_compatibility, None);
        Ok(())
    });
}

#[test]
fn load_trims_default_compatibility() {
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "  FULL  ");

        let cfg = KoraConfig::load().expect("padded valid level should load");

        assert_eq!(cfg.default_compatibility.as_deref(), Some("FULL"));
        Ok(())
    });
}

#[test]
fn load_rejects_bool_like_default_compatibility_with_friendly_error() {
    // A YAML-unquoted scalar like `true`/`123` must still reach the actionable
    // "must be one of" validation rather than an opaque type-coercion error.
    Jail::expect_with(|jail| {
        jail.set_env("DATABASE_URL", "postgres://from-env/db");
        jail.set_env("DEFAULT_COMPATIBILITY", "true");

        let err = KoraConfig::load().expect_err("bool-like value should fail load");
        let msg = err.to_string();

        assert!(msg.contains("DEFAULT_COMPATIBILITY"), "{msg}");
        assert!(msg.contains("must be one of"), "{msg}");
        Ok(())
    });
}
