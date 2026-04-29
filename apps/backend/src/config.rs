//! Typed configuration loader.
//!
//! Source order (later wins):
//! 1. baked-in defaults
//! 2. `config/relayterm.toml` (if present)
//! 3. environment variables (`RELAYTERM_*`, double-underscore = nesting)
//!
//! Only enough to boot — fields are added as the surfaces that need them
//! land.

use std::{net::SocketAddr, path::Path};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    pub(crate) server: ServerConfig,
    pub(crate) database: DatabaseConfig,
    pub(crate) dev_auth: DevAuthConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServerConfig {
    pub(crate) bind: SocketAddr,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DatabaseConfig {
    pub(crate) url: String,
    pub(crate) max_connections: u32,
}

/// Stopgap toggle for the unimplemented-auth shim.
///
/// While `enabled = true` the backend bootstraps a single hardcoded dev user
/// at startup and stamps every request with their id (see
/// `relayterm_api::DevUser`). When real auth lands the operator MUST flip
/// this to `false`; the backend will then refuse to start until the
/// bootstrap call site is removed and replaced by the session/passkey
/// middleware. The whole struct is expected to disappear at that point.
#[derive(Debug, Deserialize)]
pub(crate) struct DevAuthConfig {
    pub(crate) enabled: bool,
}

impl Config {
    pub(crate) fn load() -> anyhow::Result<Self> {
        let mut cfg = Self::defaults();

        if let Some(path) = Self::resolve_file() {
            let raw = std::fs::read_to_string(&path)?;
            let from_file: FileConfig = toml::from_str(&raw)?;
            from_file.merge_into(&mut cfg);
        }

        Self::apply_env(&mut cfg);
        Ok(cfg)
    }

    fn defaults() -> Self {
        Self {
            server: ServerConfig {
                bind: "127.0.0.1:8080"
                    .parse()
                    .expect("static default addr is valid"),
            },
            database: DatabaseConfig {
                url: "postgres://relayterm:relayterm@127.0.0.1:5432/relayterm".to_owned(),
                max_connections: 10,
            },
            dev_auth: DevAuthConfig { enabled: true },
        }
    }

    fn resolve_file() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("RELAYTERM_CONFIG") {
            let p = std::path::PathBuf::from(explicit);
            return p.is_file().then_some(p);
        }
        let candidate = Path::new("config/relayterm.toml");
        candidate.is_file().then(|| candidate.to_path_buf())
    }

    fn apply_env(cfg: &mut Self) {
        if let Ok(v) = std::env::var("RELAYTERM_SERVER__BIND")
            && let Ok(parsed) = v.parse()
        {
            cfg.server.bind = parsed;
        }
        if let Ok(v) = std::env::var("RELAYTERM_DATABASE__URL") {
            cfg.database.url = v;
        }
        if let Ok(v) = std::env::var("RELAYTERM_DATABASE__MAX_CONNECTIONS")
            && let Ok(parsed) = v.parse()
        {
            cfg.database.max_connections = parsed;
        }
        // DATABASE_URL is honored as a convenience for `sqlx-cli` parity.
        if let Ok(v) = std::env::var("DATABASE_URL") {
            cfg.database.url = v;
        }
        if let Ok(v) = std::env::var("RELAYTERM_DEV_AUTH__ENABLED")
            && let Ok(parsed) = v.parse()
        {
            cfg.dev_auth.enabled = parsed;
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    server: Option<FileServerConfig>,
    database: Option<FileDatabaseConfig>,
    dev_auth: Option<FileDevAuthConfig>,
}

#[derive(Debug, Deserialize)]
struct FileServerConfig {
    bind: Option<SocketAddr>,
}

#[derive(Debug, Deserialize)]
struct FileDatabaseConfig {
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct FileDevAuthConfig {
    enabled: Option<bool>,
}

impl FileConfig {
    fn merge_into(self, cfg: &mut Config) {
        if let Some(s) = self.server
            && let Some(bind) = s.bind
        {
            cfg.server.bind = bind;
        }
        if let Some(d) = self.database {
            if let Some(url) = d.url {
                cfg.database.url = url;
            }
            if let Some(mx) = d.max_connections {
                cfg.database.max_connections = mx;
            }
        }
        if let Some(a) = self.dev_auth
            && let Some(enabled) = a.enabled
        {
            cfg.dev_auth.enabled = enabled;
        }
    }
}
