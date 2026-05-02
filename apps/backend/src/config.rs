//! Typed configuration loader.
//!
//! Source order (later wins):
//! 1. baked-in defaults
//! 2. `config/relayterm.toml` (if present)
//! 3. environment variables (`RELAYTERM_*`, double-underscore = nesting)
//!
//! Only enough to boot — fields are added as the surfaces that need them
//! land.

use std::{fmt, net::SocketAddr, path::Path, str::FromStr};

use anyhow::{Context, anyhow, bail};
use relayterm_vault::VaultMasterKey;
use serde::Deserialize;
use zeroize::Zeroizing;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) server: ServerConfig,
    pub(crate) database: DatabaseConfig,
    pub(crate) auth: AuthConfig,
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

/// Top-level authentication mode. Decided at boot from typed config and is
/// fail-fast if misconfigured (see [`Config::validate_auth`]).
///
/// Both modes route requests through the same real-auth code path
/// (`AuthenticatedUser` extractor, password verification, opaque server-
/// side sessions). The difference is the boot-time validation envelope:
/// [`AuthMode::Production`] requires a session signing key, a non-empty
/// `allowed_origins` list, and `cookie_secure = true`; [`AuthMode::Dev`]
/// relaxes those requirements for local development. See `SPEC.md`
/// "Production authentication architecture → Auth mode model" for the
/// full contract.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AuthMode {
    #[default]
    Dev,
    Production,
}

impl AuthMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AuthMode::Dev => "dev",
            AuthMode::Production => "production",
        }
    }
}

impl FromStr for AuthMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dev" => Ok(AuthMode::Dev),
            "production" => Ok(AuthMode::Production),
            other => Err(anyhow!(
                "unrecognized auth.mode value (expected \"dev\" or \"production\"): {other:?}"
            )),
        }
    }
}

/// Production authentication configuration.
///
/// `mode` selects the validation envelope; the remaining fields shape
/// runtime auth behaviour (cookie flags, CSRF allow-list, bootstrap-
/// token policy). The session-signing key (`session_signing_key_b64` /
/// `session_signing_key_file`) is reserved for future signed-CSRF /
/// signed-cookie variants — required for `auth.mode = production` per
/// SPEC.md "Security properties to test" property 1, but not yet
/// consumed by the v1 hashed-opaque-token session model.
///
/// `Debug` is implemented manually so the secret-shaped fields
/// (`session_signing_key_b64`, `first_user_bootstrap_token`) never reach a
/// log line — only their *presence* is rendered, mirroring [`VaultConfig`].
pub(crate) struct AuthConfig {
    pub(crate) mode: AuthMode,
    pub(crate) session_signing_key_b64: Option<String>,
    pub(crate) session_signing_key_file: Option<std::path::PathBuf>,
    pub(crate) first_user_bootstrap_token: Option<String>,
    pub(crate) cookie_secure: bool,
    pub(crate) cookie_domain: Option<String>,
    pub(crate) allowed_origins: Vec<String>,
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("mode", &self.mode)
            .field(
                "session_signing_key_b64_set",
                &self.session_signing_key_b64.is_some(),
            )
            .field("session_signing_key_file", &self.session_signing_key_file)
            .field(
                "first_user_bootstrap_token_set",
                &self.first_user_bootstrap_token.is_some(),
            )
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_domain", &self.cookie_domain)
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
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

        Self::apply_env(&mut cfg)?;
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
            // Default to dev mode so local development keeps booting
            // unchanged. Production deploys MUST explicitly set
            // `auth.mode = production`, which then enforces the strict
            // boot-time envelope (session signing key, non-empty
            // `allowed_origins`, `cookie_secure = true`).
            auth: AuthConfig {
                mode: AuthMode::default(),
                session_signing_key_b64: None,
                session_signing_key_file: None,
                first_user_bootstrap_token: None,
                cookie_secure: true,
                cookie_domain: None,
                allowed_origins: Vec::new(),
            },
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

    fn apply_env(cfg: &mut Self) -> anyhow::Result<()> {
        Self::apply_env_with(cfg, |k| std::env::var(k).ok())
    }

    /// Test-friendly variant of [`Config::apply_env`] that reads variables
    /// through a caller-supplied getter rather than the global process
    /// environment. Production callers go through [`Config::apply_env`];
    /// tests inject a `HashMap`-backed closure so they can exercise env
    /// parsing without touching shared process state.
    fn apply_env_with<F>(cfg: &mut Self, getenv: F) -> anyhow::Result<()>
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(v) = getenv("RELAYTERM_SERVER__BIND")
            && let Ok(parsed) = v.parse()
        {
            cfg.server.bind = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_DATABASE__URL") {
            cfg.database.url = v;
        }
        if let Some(v) = getenv("RELAYTERM_DATABASE__MAX_CONNECTIONS")
            && let Ok(parsed) = v.parse()
        {
            cfg.database.max_connections = parsed;
        }
        // DATABASE_URL is honored as a convenience for `sqlx-cli` parity.
        if let Some(v) = getenv("DATABASE_URL") {
            cfg.database.url = v;
        }
        // Auth mode is parsed strictly: an unrecognized value is a hard
        // boot failure rather than a silent fall-through to default,
        // because misreading "production" → "dev" would silently disable
        // auth on a deploy that asked for it.
        if let Some(v) = getenv("RELAYTERM_AUTH__MODE") {
            cfg.auth.mode = AuthMode::from_str(&v).context("RELAYTERM_AUTH__MODE")?;
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64") {
            cfg.auth.session_signing_key_b64 = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE") {
            cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from(v));
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN") {
            cfg.auth.first_user_bootstrap_token = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__COOKIE_SECURE")
            && let Ok(parsed) = v.parse()
        {
            cfg.auth.cookie_secure = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__COOKIE_DOMAIN") {
            cfg.auth.cookie_domain = Some(v);
        }
        // Comma-separated list. Empty entries are dropped so a trailing
        // comma or stray whitespace does not silently widen the allow-list.
        if let Some(v) = getenv("RELAYTERM_AUTH__ALLOWED_ORIGINS") {
            cfg.auth.allowed_origins = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__ENABLED")
            && let Ok(parsed) = v.parse()
        {
            cfg.vault.enabled = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__MASTER_KEY_B64") {
            cfg.vault.master_key_b64 = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__MASTER_KEY_FILE") {
            cfg.vault.master_key_file = Some(std::path::PathBuf::from(v));
        }
        Ok(())
    }

    /// Validate the auth configuration at boot.
    ///
    /// Boot-time gate: inspects the resolved [`AuthConfig`] and refuses to
    /// proceed when it describes a state the running build cannot serve
    /// safely. Does NOT consume any secret material — secrets are still
    /// owned by `AuthConfig` after a successful return. Error messages
    /// name the failing input but never echo a value (same redaction
    /// posture as [`Config::vault_master_key`]).
    ///
    /// Cases:
    /// * `mode = dev` → always Ok. Local development picks its own
    ///   cookie/origin posture (insecure cookies, loopback origins,
    ///   missing signing key are all acceptable). The same real-auth
    ///   code path runs as in production; only the validation envelope
    ///   differs.
    /// * `mode = production` → enforce, in this order:
    ///   1. Exactly one of `session_signing_key_b64` /
    ///      `session_signing_key_file` is set (zero → reject; both →
    ///      reject as ambiguous).
    ///   2. `allowed_origins` is non-empty (an empty list rejects every
    ///      browser-write — that is the secure default the CSRF guard
    ///      already enforces, but failing fast here gives a clearer
    ///      operator signal than every POST returning 403).
    ///   3. `cookie_secure = true` (production cookies MUST carry the
    ///      `Secure` flag — there is no escape hatch).
    ///
    /// `first_user_bootstrap_token` is NOT required by this gate; the
    /// bootstrap route returns `503` when the token is unset and
    /// `409 already_bootstrapped` once a first user exists. The
    /// "production + no first user + no token" hard-fail is a runtime
    /// check in `apps/backend/src/main.rs` after the DB connect, not a
    /// config-only check (config cannot see DB state).
    pub(crate) fn validate_auth(&self) -> anyhow::Result<()> {
        match self.auth.mode {
            AuthMode::Dev => Ok(()),
            AuthMode::Production => {
                match (
                    self.auth.session_signing_key_b64.is_some(),
                    self.auth.session_signing_key_file.is_some(),
                ) {
                    (false, false) => bail!(
                        "auth.mode = production requires a session signing key — set \
                         auth.session_signing_key_b64 or auth.session_signing_key_file \
                         (RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64 / \
                         RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE)"
                    ),
                    (true, true) => bail!(
                        "auth.session_signing_key_b64 and auth.session_signing_key_file are \
                         both set; pick exactly one"
                    ),
                    _ => {}
                }
                if self.auth.allowed_origins.is_empty() {
                    bail!(
                        "auth.mode = production requires auth.allowed_origins to list at \
                         least one origin (state-changing browser-write routes reject every \
                         request when the allow-list is empty)"
                    );
                }
                if !self.auth.cookie_secure {
                    bail!(
                        "auth.mode = production requires auth.cookie_secure = true — \
                         session cookies must carry the Secure flag"
                    );
                }
                Ok(())
            }
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
    auth: Option<FileAuthConfig>,
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

/// File-side mirror of [`AuthConfig`]. `Debug` is implemented manually
/// (NOT derived) so the secret-shaped fields never reach a log line —
/// only their presence is rendered. Mirrors the redaction discipline on
/// the merged [`AuthConfig`]; without it the deserialized intermediate
/// would silently re-introduce the leak. An unrecognized `mode` value in
/// TOML is rejected by serde at deserialize time (see the
/// `#[serde(rename_all = "lowercase")]` on [`AuthMode`]).
#[derive(Deserialize)]
struct FileAuthConfig {
    mode: Option<AuthMode>,
    session_signing_key_b64: Option<String>,
    session_signing_key_file: Option<std::path::PathBuf>,
    first_user_bootstrap_token: Option<String>,
    cookie_secure: Option<bool>,
    cookie_domain: Option<String>,
    allowed_origins: Option<Vec<String>>,
}

impl fmt::Debug for FileAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileAuthConfig")
            .field("mode", &self.mode)
            .field(
                "session_signing_key_b64_set",
                &self.session_signing_key_b64.is_some(),
            )
            .field("session_signing_key_file", &self.session_signing_key_file)
            .field(
                "first_user_bootstrap_token_set",
                &self.first_user_bootstrap_token.is_some(),
            )
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_domain", &self.cookie_domain)
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
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
        if let Some(a) = self.auth {
            if let Some(mode) = a.mode {
                cfg.auth.mode = mode;
            }
            if let Some(b64) = a.session_signing_key_b64 {
                cfg.auth.session_signing_key_b64 = Some(b64);
            }
            if let Some(p) = a.session_signing_key_file {
                cfg.auth.session_signing_key_file = Some(p);
            }
            if let Some(t) = a.first_user_bootstrap_token {
                cfg.auth.first_user_bootstrap_token = Some(t);
            }
            if let Some(s) = a.cookie_secure {
                cfg.auth.cookie_secure = s;
            }
            if let Some(d) = a.cookie_domain {
                cfg.auth.cookie_domain = Some(d);
            }
            if let Some(o) = a.allowed_origins {
                cfg.auth.allowed_origins = o;
            }
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
            auth: AuthConfig {
                mode: AuthMode::Dev,
                session_signing_key_b64: None,
                session_signing_key_file: None,
                first_user_bootstrap_token: None,
                cookie_secure: true,
                cookie_domain: None,
                allowed_origins: Vec::new(),
            },
            vault: VaultConfig {
                enabled: true,
                master_key_b64: None,
                master_key_file: None,
            },
        }
    }

    /// Production-shaped config that satisfies every `validate_auth`
    /// requirement. Tests build on this and mutate the field that the
    /// case under test cares about.
    fn production_cfg() -> Config {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production;
        cfg.auth.session_signing_key_b64 = Some(BASE64_STANDARD.encode([0x42u8; 32]));
        cfg.auth.cookie_secure = true;
        cfg.auth.allowed_origins = vec!["https://relay.example.com".to_owned()];
        cfg
    }

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_owned())
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

    // --- Auth config + validation -----------------------------------

    #[test]
    fn auth_config_default_resolves_to_dev_mode() {
        let cfg = Config::defaults();
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        cfg.validate_auth().expect("default config must validate");
    }

    #[test]
    fn auth_mode_from_env_dev_validates() {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production; // ensure env genuinely overrides
        Config::apply_env_with(&mut cfg, env_from(&[("RELAYTERM_AUTH__MODE", "dev")])).unwrap();
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        cfg.validate_auth().expect("dev mode must validate");
    }

    #[test]
    fn auth_mode_dev_with_loose_settings_validates() {
        // Dev mode is the relaxed envelope: insecure cookies, empty
        // allow-list, missing signing key are all acceptable. The same
        // real-auth code path runs as in production; only the boot
        // validation differs.
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Dev;
        cfg.auth.cookie_secure = false;
        cfg.auth.allowed_origins = Vec::new();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = None;
        cfg.validate_auth().expect("dev mode must validate");
    }

    #[test]
    fn auth_mode_production_with_valid_config_validates() {
        // The canonical "production deploy" shape: signing key set,
        // non-empty allow-list, Secure cookies. Today the signing key
        // is reserved (the v1 session model uses opaque random tokens),
        // but its presence is required.
        let cfg = production_cfg();
        cfg.validate_auth()
            .expect("production with full config must validate");
    }

    #[test]
    fn auth_mode_production_missing_signing_key_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = None;
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("session signing key"),
            "error must name the missing signing key: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_both_signing_key_sources_set_is_ambiguous() {
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/dev/null"));
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("both") || msg.contains("pick exactly one"),
            "error must describe ambiguity: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_empty_allowed_origins_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.allowed_origins = Vec::new();
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("allowed_origins"),
            "error must name allowed_origins: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_cookie_secure_false_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.cookie_secure = false;
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cookie_secure"),
            "error must name cookie_secure: {msg}"
        );
        assert!(
            msg.contains("Secure"),
            "error must mention the Secure flag: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_with_signing_key_file_only_validates() {
        // The b64 source is the alternate; either branch satisfies the
        // "exactly one" requirement.
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/etc/relayterm/key"));
        cfg.validate_auth()
            .expect("production with key file only must validate");
    }

    #[test]
    fn auth_mode_production_with_optional_bootstrap_token_validates() {
        // The bootstrap token is OPTIONAL at the config-validation
        // layer; the "no first user + no token" hard-fail is a runtime
        // check in main.rs (it needs DB state). validate_auth must not
        // refuse a production config solely because the token is unset.
        let mut cfg = production_cfg();
        cfg.auth.first_user_bootstrap_token = None;
        cfg.validate_auth()
            .expect("production without bootstrap token must validate at config layer");
    }

    #[test]
    fn dev_auth_env_var_is_silently_ignored() {
        // The legacy `RELAYTERM_DEV_AUTH__ENABLED` knob no longer maps
        // to any field on `Config`. An operator who still has it set
        // should not see a config-load failure; dropping the field
        // makes the env var a no-op. Pin this so a future "let's
        // re-introduce a dev-shim toggle" PR has to delete this test
        // explicitly.
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_DEV_AUTH__ENABLED", "true")]),
        )
        .expect("legacy dev_auth env var is silently ignored");
        cfg.validate_auth()
            .expect("validation unaffected by legacy var");
    }

    #[test]
    fn legacy_dev_auth_toml_section_is_silently_ignored() {
        // Same policy at the TOML layer — an operator's stale
        // `[dev_auth]` block must not block a config load. Default
        // serde rejects unknown fields only when annotated; the
        // `FileConfig` derive does not opt in, so this is a behavior
        // test, not a structural one.
        let raw = r#"
[dev_auth]
enabled = true

[auth]
mode = "dev"
"#;
        let parsed: FileConfig = toml::from_str(raw).expect("legacy dev_auth section ignored");
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
    }

    #[test]
    fn auth_mode_invalid_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_AUTH__MODE", "totally-bogus")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_AUTH__MODE"),
            "error must name the failing input: {msg}"
        );
        // The bogus value MAY appear in the error (it's not a secret), but
        // the parse must reject the whole apply_env call so the cfg is not
        // half-mutated past this point.
    }

    #[test]
    fn auth_mode_invalid_toml_value_fails_safely() {
        let raw = "[auth]\nmode = \"halfway\"\n";
        let err = toml::from_str::<FileConfig>(raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown variant") && msg.contains("halfway"),
            "TOML parse error must name the unknown variant: {msg}"
        );
        assert!(
            msg.contains("dev") && msg.contains("production"),
            "TOML parse error must list the accepted variants: {msg}"
        );
    }

    #[test]
    fn auth_config_debug_redacts_session_signing_key_b64() {
        let cfg = AuthConfig {
            mode: AuthMode::Dev,
            session_signing_key_b64: Some("AAAA-SIGNING-KEY-MARKER-AAAA".to_owned()),
            session_signing_key_file: None,
            first_user_bootstrap_token: None,
            cookie_secure: true,
            cookie_domain: None,
            allowed_origins: Vec::new(),
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-SIGNING-KEY-MARKER-AAAA"),
            "AuthConfig debug must not echo session_signing_key_b64: {s}"
        );
        assert!(s.contains("session_signing_key_b64_set: true"));
    }

    #[test]
    fn auth_config_debug_redacts_first_user_bootstrap_token() {
        let cfg = AuthConfig {
            mode: AuthMode::Dev,
            session_signing_key_b64: None,
            session_signing_key_file: None,
            first_user_bootstrap_token: Some("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA".to_owned()),
            cookie_secure: true,
            cookie_domain: None,
            allowed_origins: Vec::new(),
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA"),
            "AuthConfig debug must not echo first_user_bootstrap_token: {s}"
        );
        assert!(s.contains("first_user_bootstrap_token_set: true"));
    }

    #[test]
    fn file_auth_config_debug_redacts_secrets() {
        let fa = FileAuthConfig {
            mode: Some(AuthMode::Production),
            session_signing_key_b64: Some("AAAA-FILE-SIGNING-KEY-MARKER-AAAA".to_owned()),
            session_signing_key_file: None,
            first_user_bootstrap_token: Some("AAAA-FILE-BOOTSTRAP-MARKER-AAAA".to_owned()),
            cookie_secure: Some(true),
            cookie_domain: None,
            allowed_origins: Some(vec!["https://relay.example.com".to_owned()]),
        };
        let s = format!("{fa:?}");
        assert!(
            !s.contains("AAAA-FILE-SIGNING-KEY-MARKER-AAAA"),
            "FileAuthConfig debug must not echo session_signing_key_b64: {s}"
        );
        assert!(
            !s.contains("AAAA-FILE-BOOTSTRAP-MARKER-AAAA"),
            "FileAuthConfig debug must not echo first_user_bootstrap_token: {s}"
        );
        assert!(s.contains("session_signing_key_b64_set: true"));
        assert!(s.contains("first_user_bootstrap_token_set: true"));
    }

    #[test]
    fn auth_validation_errors_do_not_echo_secret_env_values() {
        // A secret-shaped value supplied via env must not survive into the
        // validation error string. We pin the policy here so a future edit
        // that starts substituting the value into the error gets caught.
        // Drive every reachable production-mode failure path: missing
        // signing key, ambiguous signing key, empty allow-list, and
        // cookie_secure=false. Each case primes the secret-shaped fields
        // so the assertion is meaningful regardless of which check fires.
        const SECRET_MARKER: &str = "AAAA-SECRET-IN-ERROR-MARKER-AAAA";

        // Missing signing key — the bootstrap token is set but never
        // consulted by the validator on this path; the assertion proves
        // the validator does not opportunistically interpolate it.
        {
            let mut cfg = production_cfg();
            cfg.auth.session_signing_key_b64 = None;
            cfg.auth.session_signing_key_file = None;
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "missing-key error must not echo secret-shaped values: {err}"
            );
        }

        // Ambiguous signing key — both b64 and file are set; the b64
        // value is sentinel-shaped.
        {
            let mut cfg = production_cfg();
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/dev/null"));
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "ambiguous-key error must not echo secret-shaped values: {err}"
            );
        }

        // Empty allow-list — the bootstrap token + signing key are set
        // but the validator must still not echo them.
        {
            let mut cfg = production_cfg();
            cfg.auth.allowed_origins = Vec::new();
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "empty-allowed-origins error must not echo secret-shaped values: {err}"
            );
        }

        // cookie_secure = false — same redaction posture.
        {
            let mut cfg = production_cfg();
            cfg.auth.cookie_secure = false;
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "cookie_secure error must not echo secret-shaped values: {err}"
            );
        }
    }

    #[test]
    fn auth_env_parses_allowed_origins_csv() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_AUTH__ALLOWED_ORIGINS",
                "https://a.example.com, https://b.example.com,",
            )]),
        )
        .unwrap();
        assert_eq!(
            cfg.auth.allowed_origins,
            vec![
                "https://a.example.com".to_owned(),
                "https://b.example.com".to_owned()
            ]
        );
    }

    #[test]
    fn auth_env_parses_secret_fields_into_options() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[
                ("RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64", "k"),
                ("RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN", "t"),
                ("RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE", "/dev/null"),
                ("RELAYTERM_AUTH__COOKIE_DOMAIN", "relay.example.com"),
                ("RELAYTERM_AUTH__COOKIE_SECURE", "false"),
            ]),
        )
        .unwrap();
        assert!(cfg.auth.session_signing_key_b64.is_some());
        assert!(cfg.auth.first_user_bootstrap_token.is_some());
        assert_eq!(
            cfg.auth.session_signing_key_file.as_deref(),
            Some(std::path::Path::new("/dev/null"))
        );
        assert_eq!(cfg.auth.cookie_domain.as_deref(), Some("relay.example.com"));
        assert!(!cfg.auth.cookie_secure);
    }

    #[test]
    fn auth_config_toml_round_trip() {
        let raw = r#"
[auth]
mode = "dev"
cookie_secure = false
cookie_domain = "relay.example.com"
allowed_origins = ["https://relay.example.com"]
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        assert!(!cfg.auth.cookie_secure);
        assert_eq!(cfg.auth.cookie_domain.as_deref(), Some("relay.example.com"));
        assert_eq!(
            cfg.auth.allowed_origins,
            vec!["https://relay.example.com".to_owned()]
        );
    }
}
