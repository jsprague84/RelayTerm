#!/usr/bin/env bash
# Verify high-risk AGENTS / SPEC / docs contracts remain discoverable.
#
# Run after editing AGENTS.md, SPEC.md, docs/spec/*, or docs/agent/*.
# Read-only. No network. Standard shell tools only.

# set -e intentionally omitted: failures aggregate via fail() so the
# user sees every problem in one run instead of one-at-a-time.
set -u

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ -z "${repo_root}" ]; then
  echo "FAIL not inside a git repo" >&2
  exit 2
fi
cd "${repo_root}"

fail_count=0
checked_categories=()

note_category() {
  checked_categories+=("$1")
}

fail() {
  fail_count=$((fail_count + 1))
  echo "FAIL $*" >&2
}

# ---------------------------------------------------------------------------
# 1. Required files exist
# ---------------------------------------------------------------------------
check_files_exist() {
  note_category "required files exist"
  local f
  for f in "$@"; do
    if [ ! -f "$f" ]; then
      fail "missing required file: $f"
    fi
  done
}

REQUIRED_FILES=(
  AGENTS.md
  SPEC.md
  docs/agent/redaction-rules.md
  docs/agent/task-patterns.md
  docs/agent/encountered-lessons.md
  docs/spec/README.md
  docs/spec/auth.md
  docs/spec/auth-implementation-history.md
  docs/spec/inventory.md
  docs/spec/recording.md
  docs/spec/terminal.md
  docs/spec/terminal-adapters.md
  docs/spec/web-shell.md
  docs/terminal-recording.md
  docs/deployment/production-runbook.md
)
check_files_exist "${REQUIRED_FILES[@]}"

# ---------------------------------------------------------------------------
# 2. Anchor checks (literal substring must appear in given file)
# ---------------------------------------------------------------------------
check_anchors_in() {
  local file="$1"
  shift
  local term
  for term in "$@"; do
    if [ ! -f "$file" ]; then
      fail "anchor source missing: $file ($term)"
      continue
    fi
    if ! grep -qF -- "$term" "$file"; then
      fail "anchor missing in $file: $term"
    fi
  done
}

note_category "AGENTS.md anchors"
check_anchors_in AGENTS.md \
  "Architectural rule" \
  "Session start ritual" \
  "pinned versions" \
  "Things to avoid" \
  "Definition of done" \
  "Maintenance protocol" \
  "docs/agent/redaction-rules.md" \
  "docs/agent/task-patterns.md" \
  "docs/agent/encountered-lessons.md" \
  "SPEC.md"

note_category "SPEC.md anchors"
# Two halves of the governance line, checked independently — survives the
# line being re-formatted (e.g. bold-instead-of-italic) without dropping
# the substantive contract.
check_anchors_in SPEC.md \
  "AGENTS.md governs" \
  "SPEC.md governs" \
  "code is written" \
  "Architectural invariants" \
  "Data model" \
  "Behavior contracts" \
  "Inventory lifecycle and destructive-action policy" \
  "Integration points" \
  "Out of scope" \
  "docs/spec/README.md"

# ---------------------------------------------------------------------------
# 3. High-risk literal terms — each must appear in AT LEAST ONE corpus file
# ---------------------------------------------------------------------------
check_term_in_any() {
  local label="$1"
  local term="$2"
  shift 2
  local f
  for f in "$@"; do
    if [ -f "$f" ] && grep -qF -- "$term" "$f"; then
      return 0
    fi
  done
  fail "$label term not discoverable: $term"
}

CROSS_CORPUS=(
  AGENTS.md
  SPEC.md
  docs/agent/redaction-rules.md
  docs/spec/auth.md
  docs/spec/terminal.md
  docs/spec/terminal-adapters.md
  docs/spec/recording.md
  docs/spec/inventory.md
  docs/deployment/production-runbook.md
)

note_category "high-risk cross-corpus terms"
for term in \
  private_key \
  encrypted_private_key \
  session_token \
  token_hash \
  password_hash \
  data_b64 \
  Origin \
  CSRF \
  "tokio::spawn" \
  recording_purged \
  terminal_sessions \
  Tauri; do
  check_term_in_any "high-risk" "$term" "${CROSS_CORPUS[@]}"
done

# ---------------------------------------------------------------------------
# 4. Forbidden stale phrases — must NOT appear in current-contract docs
# ---------------------------------------------------------------------------
check_phrase_absent_in_corpus() {
  local label="$1"
  local phrase="$2"
  shift 2
  local f
  for f in "$@"; do
    if [ -f "$f" ] && grep -qF -- "$phrase" "$f"; then
      fail "$label stale phrase still present in $f: $phrase"
    fi
  done
}

note_category "forbidden stale phrases absent"
CURRENT_CONTRACT_DOCS=(AGENTS.md SPEC.md)
while IFS= read -r f; do
  CURRENT_CONTRACT_DOCS+=("$f")
done < <(find docs/spec -maxdepth 1 -type f -name '*.md' | sort)

for phrase in \
  "dev-auth is disabled" \
  "401 when dev-auth" \
  "auth handshake on the WebSocket beyond dev-auth" \
  "dev-auth gated"; do
  check_phrase_absent_in_corpus "forbidden" "$phrase" "${CURRENT_CONTRACT_DOCS[@]}"
done

# ---------------------------------------------------------------------------
# 5. Cross-file link sanity
# ---------------------------------------------------------------------------
check_file_contains() {
  local file="$1"
  local needle="$2"
  if [ ! -f "$file" ]; then
    fail "link source missing: $file"
    return
  fi
  if ! grep -qF -- "$needle" "$file"; then
    fail "expected link in $file: $needle"
  fi
}

note_category "cross-file link sanity"
check_file_contains docs/spec/recording.md "../terminal-recording.md"
check_file_contains docs/spec/README.md "auth.md"
check_file_contains docs/spec/README.md "auth-implementation-history.md"
check_file_contains docs/spec/README.md "terminal.md"
check_file_contains docs/spec/README.md "terminal-adapters.md"
check_file_contains docs/spec/terminal.md "terminal-adapters.md"
check_file_contains docs/spec/terminal-adapters.md "terminal.md"

# Conditional: if docs/deployment/docker-compose.md mentions deployment
# topics it MUST link to the production runbook. If the file is a stub
# without deployment topics yet, this check is silent.
if [ -f docs/deployment/docker-compose.md ] && \
   grep -qF "production-runbook.md" docs/deployment/docker-compose.md; then
  : # link already present, nothing to assert beyond grep above
elif [ -f docs/deployment/docker-compose.md ] && \
     grep -qiE "deployment|runbook|operator" docs/deployment/docker-compose.md; then
  fail "docs/deployment/docker-compose.md references deployment topics but does not link to production-runbook.md"
fi

# ---------------------------------------------------------------------------
# 6. Renderer production / dev-only rule discoverability
# ---------------------------------------------------------------------------
RENDERER_CORPUS=(
  docs/spec/terminal.md
  docs/spec/terminal-adapters.md
  AGENTS.md
)

note_category "renderer production/dev-only rule"
for term in \
  xterm \
  "production baseline" \
  experimental \
  dev-only \
  ghostty-web \
  restty \
  wterm; do
  check_term_in_any "renderer" "$term" "${RENDERER_CORPUS[@]}"
done

# ---------------------------------------------------------------------------
# 7. Auth contract terms discoverability
# ---------------------------------------------------------------------------
AUTH_CORPUS=(
  docs/spec/auth.md
  docs/spec/auth-implementation-history.md
  docs/agent/redaction-rules.md
)

note_category "auth contract terms"
for term in \
  bootstrap \
  cookie \
  session_token \
  token_hash \
  password_hash \
  CSRF \
  Origin \
  login_failed \
  login_succeeded \
  logout_succeeded \
  password_changed \
  session_revoked \
  sessions_revoked \
  first_user_created \
  "user exists"; do
  check_term_in_any "auth" "$term" "${AUTH_CORPUS[@]}"
done

# ---------------------------------------------------------------------------
# 8. Recording contract terms discoverability
# ---------------------------------------------------------------------------
RECORDING_CORPUS=(
  docs/spec/recording.md
  docs/terminal-recording.md
  docs/agent/redaction-rules.md
)

note_category "recording contract terms"
for term in \
  recording_purged \
  terminal_recording_chunks \
  terminal_recording_markers \
  retention \
  cleanup.enabled \
  startup_sweep_enabled \
  periodic_sweep_enabled \
  "terminal_recording.enabled" \
  data_b64; do
  check_term_in_any "recording" "$term" "${RECORDING_CORPUS[@]}"
done

# ---------------------------------------------------------------------------
# 9. Deploy config plumbing — env var × file matrix
# ---------------------------------------------------------------------------
#
# Drift class this guards: an operator-set env var is wired into
# `deploy/relayterm.env.example` and SOME (but not all) of the Compose
# templates, breaking deployment on whichever template was missed
# without any code-level test tripping. The 2026-05-09 detached-PTY-TTL
# rollout hit this — `docker-compose.example.yml` and the Traefik
# staging template were updated, but
# `docker-compose.images.example.yml` was not.
#
# Each row in the matrix below names an operator env knob and the
# files that MUST plumb it. Add a row when introducing a new operator
# env var. Per-file intentional omissions (e.g. signing key absent
# from the dev TOML on purpose) are encoded by leaving the var out of
# the relevant loop with a justifying comment — never silent.
#
# Matching is dependency-light: substring grep for compose / env files,
# word-boundary grep for the env name OR derived TOML key path OR bare
# snake_case leaf for the TOML examples.
DEPLOY_ENV_FILE=deploy/relayterm.env.example
DEPLOY_BUILD_COMPOSE=deploy/docker-compose.example.yml
DEPLOY_IMAGES_COMPOSE=deploy/docker-compose.images.example.yml
DEPLOY_TRAEFIK_COMPOSE=deploy/docker-compose.traefik-staging.example.yml
CONFIG_DEV_TOML=docs/config-examples/relayterm.dev.example.toml
CONFIG_PROD_TOML=docs/config-examples/relayterm.production.example.toml

require_substr_in_files() {
  local label="$1"
  local needle="$2"
  shift 2
  local f
  for f in "$@"; do
    if [ ! -f "$f" ]; then
      fail "deploy-plumbing[$label]: required file missing: $f"
      continue
    fi
    if ! grep -qF -- "$needle" "$f"; then
      fail "deploy-plumbing[$label]: '$needle' missing from $f (matrix requires it here)"
    fi
  done
}

# TOML mention check. Accept ANY of:
#   * full env name, e.g. RELAYTERM_AUTH__COOKIE_SECURE
#   * dotted TOML key path,   e.g. auth.cookie_secure
#   * bare snake_case leaf (word-bounded), e.g. cookie_secure
# Each TOML example is short enough that leaf collisions across
# sections are not a real risk; we accept the looser leaf form so the
# common pattern of writing `cookie_secure = true` under `[auth]`
# without repeating the env name in a comment still satisfies the
# guard.
require_toml_mention() {
  local label="$1"
  local var="$2"
  shift 2
  local key leaf
  key=$(printf '%s' "$var" | sed 's/^RELAYTERM_//' | tr '[:upper:]' '[:lower:]' | sed 's/__/./g')
  leaf=${key##*.}
  local f
  for f in "$@"; do
    if [ ! -f "$f" ]; then
      fail "deploy-plumbing[$label]: required file missing: $f"
      continue
    fi
    if grep -qF -- "$var" "$f"; then continue; fi
    if grep -qF -- "$key" "$f"; then continue; fi
    if grep -qwF -- "$leaf" "$f"; then continue; fi
    fail "deploy-plumbing[$label]: '$var' missing from $f (also tried TOML key '$key' and leaf '$leaf')"
  done
}

note_category "deploy config plumbing — backend env vars × env file + compose templates"

# Backend operator env knobs that must appear in env.example AND every
# compose template the repo ships. Add a row here when introducing a
# new operator env var — it is the entry point that prevents the
# 2026-05-09 detached-PTY-TTL drift class.
for var in \
  RELAYTERM_AUTH__MODE \
  RELAYTERM_AUTH__ALLOWED_ORIGINS \
  RELAYTERM_AUTH__COOKIE_SECURE \
  RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64 \
  RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN \
  RELAYTERM_VAULT__MASTER_KEY_B64 \
  RELAYTERM_TERMINAL_RECORDING__ENABLED \
  RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT; do
  require_substr_in_files "backend-env" "$var" \
    "$DEPLOY_ENV_FILE" \
    "$DEPLOY_BUILD_COMPOSE" \
    "$DEPLOY_IMAGES_COMPOSE" \
    "$DEPLOY_TRAEFIK_COMPOSE"
done

note_category "deploy config plumbing — postgres credentials"

# Postgres credentials. Read by every compose service that connects
# to the DB; documented in env.example. Same matrix as the backend
# env knobs above (no per-file omissions).
for var in POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB; do
  require_substr_in_files "postgres-env" "$var" \
    "$DEPLOY_ENV_FILE" \
    "$DEPLOY_BUILD_COMPOSE" \
    "$DEPLOY_IMAGES_COMPOSE" \
    "$DEPLOY_TRAEFIK_COMPOSE"
done

note_category "deploy config plumbing — image-mode-only env"

# RELAYTERM_IMAGE_TAG is image-mode-only. The build-mode compose
# template (`docker-compose.example.yml`) builds local images from
# Dockerfiles and intentionally does NOT reference the tag; the env
# file (`relayterm.env.example`) is build-mode-oriented and similarly
# omits it. Image-mode templates MUST reference it. This per-file
# difference is encoded explicitly via the file list below — do NOT
# add the build-mode compose or env file to this list without first
# wiring image-pull semantics into them.
for var in RELAYTERM_IMAGE_TAG; do
  require_substr_in_files "image-mode-env" "$var" \
    "$DEPLOY_IMAGES_COMPOSE" \
    "$DEPLOY_TRAEFIK_COMPOSE"
done

note_category "deploy config plumbing — TOML config-examples"

# Production TOML must mention every operator backend knob from the
# matrix above. The TOML uses TOML key form, env-name comments, or
# bare leaf assignments — `require_toml_mention` accepts all three.
#
# CAVEAT — short leaves (`mode`, `enabled`) are too generic to
# distinguish across TOML sections (e.g. `auth.mode`,
# `terminal_recording.encryption.mode`, `terminal_recording.compression.mode`
# all share the leaf `mode`). The current TOMLs satisfy these vars via
# the dotted-key tier (`auth.mode`, `terminal_recording.enabled`), so
# the leaf tier is dormant — but a future var with a short generic
# leaf would have weak detection in this guard. If you add such a var,
# rely on the env-name-in-comment form in the TOML rather than the
# leaf assignment alone.
for var in \
  RELAYTERM_AUTH__MODE \
  RELAYTERM_AUTH__ALLOWED_ORIGINS \
  RELAYTERM_AUTH__COOKIE_SECURE \
  RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64 \
  RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN \
  RELAYTERM_VAULT__MASTER_KEY_B64 \
  RELAYTERM_TERMINAL_RECORDING__ENABLED \
  RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT; do
  require_toml_mention "toml-prod" "$var" "$CONFIG_PROD_TOML"
done

# Dev TOML deliberately OMITS RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64
# — see `relayterm.dev.example.toml` "Session signing key is OPTIONAL
# in dev mode" comment. The omission is encoded explicitly by leaving
# it out of the dev loop below; this is the "allowlist comment, not a
# silent omission" pattern. If dev mode ever requires the signing
# key, add it back to this loop.
for var in \
  RELAYTERM_AUTH__MODE \
  RELAYTERM_AUTH__ALLOWED_ORIGINS \
  RELAYTERM_AUTH__COOKIE_SECURE \
  RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN \
  RELAYTERM_VAULT__MASTER_KEY_B64 \
  RELAYTERM_TERMINAL_RECORDING__ENABLED \
  RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER \
  RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT; do
  require_toml_mention "toml-dev" "$var" "$CONFIG_DEV_TOML"
done

# ---------------------------------------------------------------------------
# 10. Optional size info (informational only, never fails)
# ---------------------------------------------------------------------------
print_sizes() {
  local f
  for f in "$@"; do
    if [ -f "$f" ]; then
      local bytes lines
      bytes=$(wc -c <"$f" | tr -d ' ')
      lines=$(wc -l <"$f" | tr -d ' ')
      printf '  %s  %s bytes  %s lines\n' "$f" "$bytes" "$lines"
    fi
  done
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------
echo "checked categories:"
for c in "${checked_categories[@]}"; do
  echo "  - $c"
done

echo "doc sizes (informational):"
print_sizes \
  AGENTS.md \
  SPEC.md \
  docs/spec/auth.md \
  docs/spec/terminal.md \
  docs/spec/terminal-adapters.md

if [ "$fail_count" -gt 0 ]; then
  echo
  echo "docs contract check FAILED ($fail_count problem(s))" >&2
  exit 1
fi

echo
echo "docs contract check passed"
exit 0
