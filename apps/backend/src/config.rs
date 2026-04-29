//! Typed configuration loader.
//!
//! Source order (later wins):
//! 1. baked-in defaults
//! 2. `config/relayterm.toml` (if present)
//! 3. environment variables (`RELAYTERM_*`, double-underscore = nesting)
//!
//! Only enough to boot — fields are added as the surfaces that need them
//! land.

use std::{fmt, net::SocketAddr, path::Path};

use anyhow::{Context, anyhow, bail};
use relayterm_vault::VaultMasterKey;
use serde::Deserialize;
use zeroize::Zeroizing;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) server: ServerConfig,
    pub(crate) database: DatabaseConfig,
    pub(crate) dev_auth: DevAuthConfig,
    pub(crate) vault: VaultConfig,
}

#[derive(Debug)]
pub(crate) struct ServerConfig {
    pub(crate) bind: SocketAddr,
}

/// `Debug` is implemented manually so the password segment of the
/// Postgres URL never reaches a log line. A typical operator URL looks
/// like `postgres://relayterm:s3cret@host:5432/db` — the `s3cret`
/// segment is masked while host/db stay visible for diagnostics.
pub(crate) struct DatabaseConfig {
    pub(crate) url: String,
    pub(crate) max_connections: u32,
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &format_args!("{}", redact_database_url(&self.url)))
            .field("max_connections", &self.max_connections)
            .finish()
    }
}

/// Mask the password portion of a `scheme://user:password@host/...` URL.
///
/// Returns the URL unchanged if no `://` or no `@` is present (not a
/// password-bearing form), and replaces the password between `:` and `@`
/// with `***`. This is a structural mask, not a parser — the goal is to
/// keep `Debug` output safe without dragging in a URL crate, not to
/// validate the URL.
fn redact_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let after_scheme_at = scheme_end + 3;
    let rest = &url[after_scheme_at..];
    let Some(at_pos) = rest.find('@') else {
        return url.to_owned();
    };
    let userinfo = &rest[..at_pos];
    let host_and_path = &rest[at_pos..];
    let masked_userinfo = match userinfo.find(':') {
        Some(colon) => format!("{}:***", &userinfo[..colon]),
        None => userinfo.to_owned(),
    };
    format!(
        "{}://{}{}",
        &url[..scheme_end],
        masked_userinfo,
        host_and_path
    )
}

/// Stopgap toggle for the unimplemented-auth shim.
///
/// While `enabled = true` the backend bootstraps a single hardcoded dev user
/// at startup and stamps every request with their id (see
/// `relayterm_api::DevUser`). When real auth lands the operator MUST flip
/// this to `false`; the backend will then refuse to start until the
/// bootstrap call site is removed and replaced by the session/passkey
/// middleware. The whole struct is expected to disappear at that point.
#[derive(Debug)]
pub(crate) struct DevAuthConfig {
    pub(crate) enabled: bool,
}

/// Vault (encrypted private-key store) configuration.
///
/// Exactly one of `master_key_b64` or `master_key_file` must resolve at
/// boot. `Debug` is implemented manually so the resolved key bytes never
/// reach a log line — only the *presence* of each source is rendered.
///
/// Boot rules (enforced by [`Config::vault_master_key`]):
/// * If `enabled` is `true` and no source resolves → startup fails.
/// * Both sources set → startup fails (ambiguous).
/// * No silent fallback to a randomly generated key.
/// * Error messages mention which source failed (file vs. base64) but
///   never echo the value or any prefix of it.
pub(crate) struct VaultConfig {
    pub(crate) enabled: bool,
    pub(crate) master_key_b64: Option<String>,
    pub(crate) master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for VaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultConfig")
            .field("enabled", &self.enabled)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
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
            vault: VaultConfig {
                enabled: true,
                master_key_b64: None,
                master_key_file: None,
            },
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
        if let Ok(v) = std::env::var("RELAYTERM_VAULT__ENABLED")
            && let Ok(parsed) = v.parse()
        {
            cfg.vault.enabled = parsed;
        }
        if let Ok(v) = std::env::var("RELAYTERM_VAULT__MASTER_KEY_B64") {
            cfg.vault.master_key_b64 = Some(v);
        }
        if let Ok(v) = std::env::var("RELAYTERM_VAULT__MASTER_KEY_FILE") {
            cfg.vault.master_key_file = Some(std::path::PathBuf::from(v));
        }
    }

    /// Resolve the configured master key, or return `None` when the vault
    /// is intentionally disabled. Consumes the configured key sources so
    /// the raw base64 string does not linger on the heap for the process
    /// lifetime — `take()`d into a `Zeroizing<String>` that wipes itself
    /// when this function returns.
    ///
    /// Failure modes (each returns a descriptive error that does NOT echo
    /// the key value):
    /// * vault enabled and neither source set → "no master key configured"
    /// * vault enabled and both sources set → "ambiguous master key"
    /// * source set but unreadable / invalid → wraps the structural reason
    pub(crate) fn vault_master_key(&mut self) -> anyhow::Result<Option<VaultMasterKey>> {
        if !self.vault.enabled {
            // Drop any configured sources up front so a disabled vault
            // never keeps the raw key on the heap.
            let _ = self.vault.master_key_b64.take().map(Zeroizing::new);
            self.vault.master_key_file.take();
            return Ok(None);
        }
        let b64 = self.vault.master_key_b64.take().map(Zeroizing::new);
        let path = self.vault.master_key_file.take();
        match (b64, path) {
            (Some(_), Some(_)) => bail!(
                "vault.master_key_b64 and vault.master_key_file are both set; pick exactly one"
            ),
            (Some(b64), None) => VaultMasterKey::from_base64(&b64)
                .map(Some)
                .map_err(|e| anyhow!("vault.master_key_b64 invalid: {e}")),
            (None, Some(path)) => VaultMasterKey::from_file(&path)
                .map(Some)
                .with_context(|| format!("vault.master_key_file at {}", path.display())),
            (None, None) => bail!(
                "vault.enabled = true but no master key configured (set vault.master_key_b64 \
                 or vault.master_key_file, or flip vault.enabled = false to opt out)"
            ),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    server: Option<FileServerConfig>,
    database: Option<FileDatabaseConfig>,
    dev_auth: Option<FileDevAuthConfig>,
    vault: Option<FileVaultConfig>,
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

/// File-side mirror of [`VaultConfig`]. `Debug` is implemented manually
/// (NOT derived) so the raw base64 master key never reaches a log line —
/// only its presence is rendered. Mirrors the redaction discipline on the
/// merged [`VaultConfig`]; without it the deserialized intermediate would
/// silently re-introduce the leak.
#[derive(Deserialize)]
struct FileVaultConfig {
    enabled: Option<bool>,
    master_key_b64: Option<String>,
    master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for FileVaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileVaultConfig")
            .field("enabled", &self.enabled)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
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
        if let Some(v) = self.vault {
            if let Some(enabled) = v.enabled {
                cfg.vault.enabled = enabled;
            }
            if let Some(b64) = v.master_key_b64 {
                cfg.vault.master_key_b64 = Some(b64);
            }
            if let Some(p) = v.master_key_file {
                cfg.vault.master_key_file = Some(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    fn empty_cfg() -> Config {
        Config {
            server: ServerConfig {
                bind: "127.0.0.1:8080".parse().unwrap(),
            },
            database: DatabaseConfig {
                url: "x".to_owned(),
                max_connections: 1,
            },
            dev_auth: DevAuthConfig { enabled: true },
            vault: VaultConfig {
                enabled: true,
                master_key_b64: None,
                master_key_file: None,
            },
        }
    }

    #[test]
    fn vault_config_debug_redacts_master_key_b64() {
        let cfg = VaultConfig {
            enabled: true,
            master_key_b64: Some("AAAA-MASTER-KEY-MARKER-AAAA".to_owned()),
            master_key_file: None,
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-MASTER-KEY-MARKER-AAAA"),
            "VaultConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn file_vault_config_debug_redacts_master_key_b64() {
        let fv = FileVaultConfig {
            enabled: Some(true),
            master_key_b64: Some("AAAA-FILE-MASTER-KEY-MARKER-AAAA".to_owned()),
            master_key_file: None,
        };
        let s = format!("{fv:?}");
        assert!(
            !s.contains("AAAA-FILE-MASTER-KEY-MARKER-AAAA"),
            "FileVaultConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn vault_master_key_consumes_b64_source_on_success() {
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0x42u8; 32]));

        let key = cfg.vault_master_key().unwrap();
        assert!(key.is_some(), "valid b64 key should resolve");
        assert!(
            cfg.vault.master_key_b64.is_none(),
            "raw b64 source must be wiped from Config after key resolution"
        );
        assert!(cfg.vault.master_key_file.is_none());
    }

    #[test]
    fn vault_master_key_consumes_b64_source_on_failure() {
        // Even on a decode failure the source must not linger — `take`
        // happens unconditionally so the heap copy gets dropped.
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some("not-valid-base64-!@#$".to_owned());

        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string().contains("vault.master_key_b64 invalid"),
            "error should name the failing source: {err}"
        );
        assert!(
            cfg.vault.master_key_b64.is_none(),
            "failed b64 source must still be wiped from Config"
        );
    }

    #[test]
    fn vault_master_key_disabled_drops_sources_without_resolving() {
        let mut cfg = empty_cfg();
        cfg.vault.enabled = false;
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0u8; 32]));
        cfg.vault.master_key_file = Some(std::path::PathBuf::from("/dev/null"));

        let key = cfg.vault_master_key().unwrap();
        assert!(key.is_none());
        assert!(cfg.vault.master_key_b64.is_none());
        assert!(cfg.vault.master_key_file.is_none());
    }

    #[test]
    fn vault_master_key_rejects_both_sources_set() {
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0u8; 32]));
        cfg.vault.master_key_file = Some(std::path::PathBuf::from("/dev/null"));

        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string()
                .contains("master_key_b64 and vault.master_key_file are both set"),
            "error should describe ambiguity: {err}"
        );
    }

    #[test]
    fn vault_master_key_requires_a_source_when_enabled() {
        let mut cfg = empty_cfg();
        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string().contains("no master key configured"),
            "error should explain missing key: {err}"
        );
    }

    #[test]
    fn database_url_debug_redacts_password() {
        let db = DatabaseConfig {
            url: "postgres://relayterm:s3cret-passw0rd@db.internal:5432/relayterm".to_owned(),
            max_connections: 10,
        };
        let s = format!("{db:?}");
        assert!(
            !s.contains("s3cret-passw0rd"),
            "database url debug must not contain the password: {s}"
        );
        // Diagnostically useful pieces should survive: host, port, db name,
        // username, and the masking marker so it's clear redaction happened.
        assert!(s.contains("relayterm:***"));
        assert!(s.contains("db.internal:5432/relayterm"));
    }

    #[test]
    fn database_url_redaction_passthrough_on_passwordless_url() {
        // A URL without userinfo or a password segment should round-trip
        // unchanged so operators can still see the bind detail.
        assert_eq!(
            redact_database_url("postgres://db.internal:5432/relayterm"),
            "postgres://db.internal:5432/relayterm"
        );
        assert_eq!(
            redact_database_url("postgres://relayterm@db.internal:5432/relayterm"),
            "postgres://relayterm@db.internal:5432/relayterm"
        );
    }

    #[test]
    fn database_url_redaction_handles_non_url_strings() {
        // If the value isn't a `scheme://...` form, leave it alone — the
        // mask is a safety net, not a parser.
        assert_eq!(redact_database_url("not-a-url"), "not-a-url");
    }
}
