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
    pub(crate) dev_auth: DevAuthConfig,
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

/// Top-level authentication mode. Decided at boot from typed config and is
/// fail-fast if misconfigured (see [`Config::validate_auth`]).
///
/// Today only [`AuthMode::Dev`] resolves to a working backend.
/// [`AuthMode::Production`] is a reserved value that refuses to boot — the
/// production auth path (sessions, password verification, login routes,
/// `AuthenticatedUser` extractor) is not yet implemented. The mode lives in
/// config so future production-auth slices land without flipping a build
/// flag and so a deploy can be rejected at startup BEFORE it accepts traffic
/// without auth. See `SPEC.md` "Production authentication architecture →
/// Auth mode model" for the full contract.
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
/// All fields except `mode` are reserved for upcoming auth slices and are
/// NOT consumed today — the production code path is rejected at boot via
/// [`Config::validate_auth`]. The fields exist so operators can shape their
/// config files / environment against the final names while real auth is
/// being built; per `SPEC.md` "Auth mode model" the reserved keys are
/// `mode`, `session_signing_key_b64`, `session_signing_key_file`,
/// `first_user_bootstrap_token`, `cookie_secure`, `cookie_domain`, and
/// `allowed_origins`.
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
            dev_auth: DevAuthConfig { enabled: true },
            // Default to dev mode so existing local development keeps
            // booting unchanged. Production deploys MUST explicitly set
            // `auth.mode = production` once a future slice implements the
            // production path; today that selection is rejected at boot.
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
        if let Some(v) = getenv("RELAYTERM_DEV_AUTH__ENABLED")
            && let Ok(parsed) = v.parse()
        {
            cfg.dev_auth.enabled = parsed;
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
    /// Boot-time gate: inspects the resolved [`AuthConfig`] +
    /// [`DevAuthConfig`] combination and refuses to proceed when they
    /// describe a state the running build cannot serve safely. Does NOT
    /// consume any secret material — secrets are still owned by `AuthConfig`
    /// after a successful return so a future slice can move them into the
    /// auth service. Error messages name the failing input but never echo
    /// any value (same redaction posture as [`Config::vault_master_key`]).
    ///
    /// Cases:
    /// * `mode = dev` → always Ok. Whether `dev_auth.enabled = true` or
    ///   `false` is up to the caller (the existing `main.rs` warn-line
    ///   covers the latter — the API is unprotected because no auth source
    ///   is wired). This matches `SPEC.md` "Security properties to test"
    ///   property 1.
    /// * `mode = production` AND `dev_auth.enabled = true` → reject. The
    ///   two flags are mutually exclusive (`SPEC.md` "Auth mode model"
    ///   rule 3). This case gets its own error so an operator who flipped
    ///   `auth.mode = production` without flipping `dev_auth.enabled` sees
    ///   exactly which input is wrong.
    /// * `mode = production` (any `dev_auth.enabled`) → reject. Production
    ///   auth (sessions, password verification, login routes,
    ///   `AuthenticatedUser` extractor) is not implemented in this build
    ///   yet. Failing fast here is the load-bearing guarantee that a deploy
    ///   never silently runs without auth.
    pub(crate) fn validate_auth(&self) -> anyhow::Result<()> {
        match self.auth.mode {
            AuthMode::Dev => Ok(()),
            AuthMode::Production => {
                if self.dev_auth.enabled {
                    bail!(
                        "auth.mode = production is mutually exclusive with \
                         dev_auth.enabled = true (set dev_auth.enabled = false \
                         or RELAYTERM_DEV_AUTH__ENABLED=false)"
                    );
                }
                bail!(
                    "auth.mode = production is not implemented in this build yet — \
                     production authentication (sessions, password verification, \
                     login routes, AuthenticatedUser extractor) requires future \
                     implementation slices. Set auth.mode = dev for local \
                     development, or pin a build that ships production auth."
                )
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
    dev_auth: Option<FileDevAuthConfig>,
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

#[derive(Debug, Deserialize)]
struct FileDevAuthConfig {
    enabled: Option<bool>,
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
        if let Some(a) = self.dev_auth
            && let Some(enabled) = a.enabled
        {
            cfg.dev_auth.enabled = enabled;
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
            dev_auth: DevAuthConfig { enabled: true },
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
        assert!(
            cfg.dev_auth.enabled,
            "dev_auth.enabled stays true by default"
        );
        cfg.validate_auth().expect("default config must validate");
    }

    #[test]
    fn auth_mode_from_env_dev_validates() {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production; // ensure env genuinely overrides
        Config::apply_env_with(&mut cfg, env_from(&[("RELAYTERM_AUTH__MODE", "dev")])).unwrap();
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        cfg.validate_auth()
            .expect("dev mode + dev_auth.enabled = true must validate");
    }

    #[test]
    fn auth_mode_from_env_production_fails_fast() {
        let mut cfg = empty_cfg();
        cfg.dev_auth.enabled = false;
        Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_AUTH__MODE", "production")]),
        )
        .unwrap();
        assert_eq!(cfg.auth.mode, AuthMode::Production);
        let err = cfg.validate_auth().unwrap_err();
        assert!(
            err.to_string().contains("not implemented"),
            "production mode must fail fast until real auth lands: {err}"
        );
    }

    #[test]
    fn auth_mode_production_with_dev_auth_enabled_is_explicit_conflict() {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production;
        cfg.dev_auth.enabled = true;
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "production + dev_auth.enabled must surface the explicit conflict: {msg}"
        );
        // The conflict error must be distinct from the not-implemented
        // error so an operator can tell which knob to flip.
        assert!(
            !msg.contains("not implemented"),
            "conflict path must not collapse into the not-implemented message: {msg}"
        );
    }

    #[test]
    fn auth_mode_dev_with_dev_auth_disabled_validates() {
        // Per SPEC.md "Security properties to test" #1:
        // `auth.mode = dev` with `dev_auth.enabled = false` is allowed but
        // logs a warning that the API is unprotected. The warn-line itself
        // is emitted by main.rs; this test pins the policy.
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Dev;
        cfg.dev_auth.enabled = false;
        cfg.validate_auth()
            .expect("dev mode + dev_auth.enabled = false must validate");
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
        const SECRET_MARKER: &str = "AAAA-SECRET-IN-ERROR-MARKER-AAAA";
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production;
        cfg.dev_auth.enabled = true;
        cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
        cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains(SECRET_MARKER),
            "validation error must not echo secret-shaped values: {msg}"
        );
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
