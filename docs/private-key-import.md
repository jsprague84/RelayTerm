# Private-key import — design

> Design doc for importing an existing SSH private key into RelayTerm's
> backend-managed vault. **Status: v1 implemented on
> `feat/private-key-import-v1` (2026-05-12).** The route, the vault
> primitive, the audit-kind reuse + `source` discriminator, the
> duplicate-fingerprint mapping, the SPA helper + Identities-view
> Import panel, and the redaction-sentinel tests are all wired against
> the design below; the deferred sections (§ 10, § 13.1 / § 13.2,
> § 14) remain accurate as the v1.1+ roadmap. Sibling docs and
> `SPEC.md` "SSH credential and trust surfaces" reflect the landed
> shape.
>
> Sibling docs:
>
> - `crates/relayterm-vault/src/{lib,identity,cipher}.rs` — the vault
>   primitives this design builds on (envelope shape, `EncryptedBlob`,
>   `VaultService::generate_ssh_identity`).
> - `crates/relayterm-api/src/routes/v1/ssh_identities.rs` — the existing
>   `POST` / `PATCH` / `DELETE` / `GET` shape that import must mirror.
> - `docs/agent/redaction-rules.md` § 1 (audit payloads) and § 3
>   (inventory destructive-action policy) — the load-bearing redaction
>   contracts.
> - `docs/spec/inventory.md` — "Production SSH identity generation UI"
>   ↔ future "Private-key import UI" parity contract.
> - `SPEC.md` — "Inventory lifecycle and destructive-action policy" for
>   audit kinds, and "Out of scope (v1)" / "Open questions" entries.

## 1. Summary of decisions

| Decision | Position |
|---|---|
| **v1 supported formats** | OpenSSH private key, **Ed25519 only, unencrypted** (no passphrase). PEM PKCS#1 / PKCS#8 / Putty `.ppk` are out of scope. RSA / ECDSA / DSA are out of scope. |
| **Passphrase-protected keys** | **Deferred to v1.1.** v1 returns a typed `400 unsupported_key_format { reason: "encrypted" }` if the supplied bytes parse as an encrypted OpenSSH key. Adding decrypt support requires enabling `ssh-key`'s `encryption` cipher features and a careful HTTPS-only one-shot passphrase channel — large enough that bundling it into v1 widens the security review surface. v1.1 is the natural follow-up. |
| **API endpoint** | `POST /api/v1/ssh-identities/import`. New route, parallel to `POST /api/v1/ssh-identities`. The existing generate route is unchanged. |
| **Request body** | `{ name: string, private_key_openssh: string }`. No `private_key_pem` field in v1. No `passphrase` field in v1 (added in v1.1). |
| **Response** | The existing `SshIdentityResponse` — `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. Identical shape to the generate path so list / parse / preview helpers are reused unchanged. |
| **Frontend UX (v1)** | Paste textarea (no file picker in v1). The "Generate" panel and a new sibling "Import" panel live on `IdentitiesView.svelte` behind separate headers. |
| **Vault behavior** | Backend parses the supplied OpenSSH PEM, validates the algorithm, derives the public key + fingerprint, re-serializes the private key as canonical OpenSSH PEM, encrypts via the existing `cipher::encrypt(master_key, plaintext)` envelope, persists exactly the same row shape as a generated identity. |
| **Audit** | Reuse the existing `ssh_identity_created` audit kind (no new migration). Payload adds a single discriminator field: `source: "generated" \| "imported"`. Public metadata only. |
| **Duplicate-fingerprint policy** | Already enforced structurally by the schema's `UNIQUE (owner_id, fingerprint_sha256)` index. The route collapses the FK violation to `409 conflict { entity: "ssh_identity", reason: "duplicate_fingerprint" }` *before* the audit append (idempotency keystone, mirrors § 2 of the redaction rules). |

This v1 scope deliberately matches the existing generate flow's surface
exactly. Anything that would require either a new audit kind, a new
schema column, a passphrase channel, a new key algorithm, a file-picker
DOM surface, or a new error envelope shape is in **§ 13 Out of scope**
or **§ 14 Open questions** — not v1.

## 2. Threat model and invariants

The change must preserve every invariant that already protects the
backend-generated path. Specifically:

1. **The plaintext private key never reaches the browser.** No code path
   in this design returns it; the response DTO has no field that could
   carry it. The redaction tests on `parseSshIdentity` (no
   `private_key` / `encrypted_private_key` property) cover both routes.
2. **The plaintext private key is never logged.** No `tracing::*` line
   in the import path sees the request body, the parsed PEM bytes, the
   `ssh-key` error text, or the derived bytes. Operator-side `warn!`
   lines name only the rejected discriminant (e.g.
   `"private_key_import: unsupported_key_type"`), never the input bytes.
3. **The plaintext private key never reaches durable storage.** It
   exists only inside `VaultService::import_ssh_identity`, wrapped in
   `Zeroizing<Vec<u8>>`, and is dropped before the function returns.
   Only the ciphertext blob (existing `EncryptedBlob` envelope, magic
   `RTV1` + version `0x01`) is persisted.
4. **The plaintext private key never reaches the audit log.** The
   `ssh_identity_created` payload is built field-by-field (mirror of
   `write_ssh_identity_delete_audit`) and carries `ssh_identity_id`,
   `name`, `key_type`, `fingerprint_sha256`, `created_at`, and the new
   `source` discriminator. The `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel
   tests are the redaction backstop (see § 11).
5. **The plaintext private key never reaches an error response body.**
   `ssh-key` parse failures map to a small, stable enum of reasons
   (`malformed`, `unsupported_key_type`, `encrypted`, `not_a_private_key`).
   The raw parser text never crosses the API boundary.
6. **The passphrase, if and when introduced in v1.1, is one-shot.** It
   crosses the HTTPS boundary exactly once in the request body, is held
   in a `Zeroizing<String>`, is used to decrypt, and is dropped. It is
   never stored, never logged, never returned, and never echoed. The
   wire shape DOES NOT include a `passphrase` field in v1; it is added
   in v1.1 only.
7. **A passphrase-protected key in v1 must be rejected explicitly**
   rather than parsed as an unencrypted key by accident. The parser
   must detect the encrypted-header signature (`-----BEGIN OPENSSH
   PRIVATE KEY-----` is the same header regardless, so detection is by
   `ssh-key`'s `PrivateKey::is_encrypted()` check, not by header
   sniffing) and return a typed `unsupported_format { reason:
   "encrypted" }` 400. This requires adding a new
   `VaultError::UnsupportedFormat { reason: &'static str }` variant
   alongside the existing `UnsupportedKeyType(&'static str)` — see
   § 5.1 for the closed `reason` enum and § 13 for the implementation
   order.
8. **CSRF / Origin / Authenticated user** — the new route inherits the
   existing posture exactly. Handler signature places `_csrf:
   CsrfGuard` ahead of `Json<...>`; the route is owner-scoped via
   `AuthenticatedUser::user_id()`. No deviation from the patterns
   captured in `docs/agent/redaction-rules.md` §§ 7-8.

## 3. v1 supported formats — rationale

**The single supported shape in v1 is an unencrypted OpenSSH-format
Ed25519 private key.** This is the format an operator gets from
`ssh-keygen -t ed25519` and a copy of `~/.ssh/id_ed25519`. The header is
`-----BEGIN OPENSSH PRIVATE KEY-----` and the body is the
`openssh-key-v1` binary blob, base64-encoded.

### Why only Ed25519 in v1

- The vault's existing generator (`VaultService::generate_ssh_identity`)
  supports Ed25519 only. The wire-stable `SshKeyType` union has
  variants for `Rsa` / `EcdsaP256` / `EcdsaP384` / `EcdsaP521` so the
  read path can decode legacy rows, but the generation surface is
  intentionally narrow. Import keeps that intersection.
- `ssh-key 0.6` is pinned with `features = ["alloc", "ed25519"]` in
  `Cargo.toml`. Parsing RSA or ECDSA requires the `rsa` / `p256` /
  `p384` / `p521` features, which pull in larger transitive crates
  (`rsa`, `rsa-oaep`-adjacent code) and a substantially wider audit
  surface. Adding them is a separate, deliberate slice that goes
  through Stack-table review (see `AGENTS.md` § Stack).
- An operator who already has an RSA key today is best served by
  generating a fresh Ed25519 keypair via the existing UI — Ed25519
  beats RSA on every dimension that matters in 2026 (size, speed,
  side-channel resistance). v1.1 / v1.2 may add RSA-3072+ import as a
  bridge for legacy `authorized_keys` constraints, but only behind a
  documented operator policy choice.

### Why not PEM PKCS#1 / PKCS#8

- A user's `~/.ssh/id_*` is OpenSSH-format by default since OpenSSH
  7.8 (2018). PEM PKCS#1 / PKCS#8 keys are mostly seen in code-signing
  / TLS contexts; importing them as SSH credentials confuses the
  format-vs-purpose boundary.
- `ssh-key 0.6` has a separate code path for PEM keys (`from_pem`)
  with its own algorithm gating. Supporting it in v1 doubles the
  parser surface for marginal benefit.

### Why not Putty `.ppk`

- Putty's format is a distinct serialisation with its own
  HMAC-protected layout. Supporting it requires a new parser
  dependency. Out of scope for v1; can be added later behind a
  feature gate without changing the wire shape (the format is a
  parsing artefact, not a wire artefact — `private_key_openssh` would
  become `private_key_openssh | private_key_ppk` as a tagged union or
  `format: "openssh" | "ppk"` discriminator).

### Why not encrypted (passphrase-protected) keys in v1

- The OpenSSH encrypted-private-key path requires `ssh-key`'s
  `encryption` feature plus a cipher backend
  (`aes-cbc` / `aes-ctr` / `aes-gcm`). Enabling them widens the
  transitive crypto crate surface.
- A passphrase channel needs its own redaction discipline: it
  shape-aliases a password and must therefore be treated like one —
  one-shot, never stored, never logged, never echoed, zeroized in
  memory. That is implementable, but it is non-trivial and benefits
  from being landed as a focused v1.1 slice with its own tests rather
  than bundled into v1.
- The v1 detection-and-reject behavior is **already** the right thing
  for v1 even if v1.1 ships shortly: an explicit `400
  unsupported_format { reason: "encrypted" }` is more useful to the
  operator than a `500` from a parser feature mismatch.

## 4. API shape

### 4.1 New route

```
POST /api/v1/ssh-identities/import
```

- Extractors (handler signature, in source order):
  `_csrf: CsrfGuard`, `user: AuthenticatedUser`, `State(state):
  State<AppState>`, `Json(req): Json<ImportSshIdentityRequest>`.
- The CSRF guard runs before the body extractor by axum 0.8 extractor
  ordering (see redaction-rules § 7). An integration test mirroring
  `bad_origin_rejects_before_body_parsing` covers it.
- Success: `201 Created` with a `SshIdentityResponse` body
  (identical to the generate route).
- All error paths use the existing `ApiError` enum and the existing
  `{ error: { code, message } }` envelope.

### 4.2 Request body

```rust
// crates/relayterm-api/src/dto/ssh_identity.rs (new sibling DTO)
//
// NOTE: derived `Debug` is FORBIDDEN here. The existing
// `CreateSshIdentityRequest` next to this one derives `Debug` safely
// because every field is public user-supplied metadata; that does NOT
// hold for `private_key_openssh`. A derived `Debug` would emit the
// full PEM bytes through any `dbg!`, `format!("{:?}")`, or tracing
// subscriber that formats the type. Implement a manual `Debug` that
// redacts `private_key_openssh` to `<redacted: N bytes>`, mirror of
// `EncryptedBlob` and `SshIdentity::encrypted_private_key`.
#[derive(Deserialize)]
pub(crate) struct ImportSshIdentityRequest {
    pub name: String,
    /// OpenSSH-format private key text. Must be ASCII; size-bounded by
    /// the axum `RequestBodyLimit` layer (the import body is the
    /// reason that limit may need a deliberate bump — see § 4.4).
    pub private_key_openssh: String,
    // NOTE: `passphrase` is intentionally absent from v1. v1.1 adds it
    //       as `pub passphrase: Option<String>` and treats it as
    //       `Zeroizing<String>` immediately on entry.
}

impl fmt::Debug for ImportSshIdentityRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportSshIdentityRequest")
            .field("name", &self.name)
            .field(
                "private_key_openssh",
                &format_args!("<redacted: {} bytes>", self.private_key_openssh.len()),
            )
            .finish()
    }
}
```

The validator MUST consume `private_key_openssh` by **move** (not
borrow) when handing it to `VaultService::import_ssh_identity`,
re-wrap it as `Zeroizing<Vec<u8>>` at the boundary, and drop the
original `String` allocation before the handler returns. The DTO
struct holds the bytes for as little time as possible — between
deserialization and the validator's move into the vault call. A
`warn!` that names a rejected discriminant (e.g.
`"unsupported_key_type"`) is allowed; a `warn!` that includes the
request body is forbidden.

Validation rules (mirror `validate_identity_name` for the name plus
new rules for the PEM):

- `name`: shared with the generate path — uses the existing
  `validate_identity_name` helper byte-for-byte.
- `private_key_openssh`:
  - ASCII only (reject any byte > 0x7E or < 0x09).
  - Length ≤ a small upper bound (proposal: **8 KiB** — an Ed25519
    OpenSSH PEM is ~400 bytes; 8 KiB is well above plausible legitimate
    sizes and well below the default axum body limit, so a malformed
    paste does not chew CPU in the parser).
  - Must contain the OpenSSH header sentinel
    `-----BEGIN OPENSSH PRIVATE KEY-----`. (PEM-shape pre-check; the
    real parse is `ssh-key`'s job.)
- All validation errors map to a single `ApiError::Validation` shape
  with a stable message — `"name must not be empty"`,
  `"private_key_openssh must be ASCII"`, `"private_key_openssh must
  not exceed 8 KiB"`, `"private_key_openssh is missing OpenSSH PEM
  header"`. Never echo the offending input.

### 4.3 Response

Identical shape to `POST /api/v1/ssh-identities`:

```jsonc
{
  "id": "…uuid…",
  "name": "homelab-admin",
  "key_type": "ed25519",
  "public_key": "ssh-ed25519 AAAA… homelab-admin",
  "fingerprint_sha256": "SHA256:…",
  "created_at": "2026-05-12T…Z",
  "last_used_at": null
}
```

`parseSshIdentity` is the existing TypeScript guard and is reused
unchanged — the import path inherits the no-`private_key` /
no-`encrypted_private_key` redaction property for free.

### 4.4 Request body size

Pasting an Ed25519 OpenSSH key is ~400 bytes. An RSA-4096 OpenSSH key
is ~3.3 KiB. The proposed **8 KiB** per-request cap is comfortably
above both and below the default axum / hyper body cap. The cap is
enforced inside `ImportSshIdentityRequest::validate` (post-deserialize,
pre-parse) so the bound is explicit and DTO-local.

A future `private_key_pem` / `private_key_ppk` extension would re-use
the same cap — 8 KiB is generous for every realistic input.

## 5. Backend processing pipeline

The route handler is short and follows the existing generate-route
pattern:

```
1. CsrfGuard runs (axum extractor ordering).
2. AuthenticatedUser resolves the caller (401 otherwise).
3. ImportSshIdentityRequest deserializes from the request body.
4. req.validate() runs (name + PEM shape + size cap).
5. state.vault.import_ssh_identity(&validated, &owner_id) runs in the
   vault crate:
   a. PrivateKey::from_openssh(validated.pem) — parsing.
   b. If is_encrypted() → return VaultError::UnsupportedFormat { reason:
      "encrypted" }.
   c. Algorithm must be Algorithm::Ed25519 → otherwise
      VaultError::UnsupportedKeyType("…").
   d. The name (validated, trimmed) is applied as the OpenSSH comment
      via private.set_comment(name) so the re-serialized PEM matches
      the generated-path convention (operator-visible identity name in
      the `authorized_keys` line).
   e. public_key_openssh = private.public_key().to_openssh().
   f. fingerprint_sha256 = private.public_key().fingerprint(Sha256).
   g. canonical_pem = private.to_openssh(LineEnding::LF) — wrapped in
      Zeroizing<Vec<u8>>. The original validated.pem is also dropped.
   h. encrypted_private_key = cipher::encrypt(&self.master_key,
      &canonical_pem).
   i. Returns GeneratedSshIdentity (the existing struct shape; no new
      domain type).
6. state.db.ssh_identities().create(CreateSshIdentity { … }) inserts
   the row. A unique-violation on (owner_id, fingerprint_sha256) maps
   to ApiError::Conflict { entity: "ssh_identity", reason:
   "duplicate_fingerprint" }.
7. write_ssh_identity_create_audit(&state, user_id, &identity,
   AuditSource::Imported) appends one ssh_identity_created row.
8. Response: 201 Created + Json(SshIdentityResponse::from(identity)).
```

Steps 5(b), 5(c), and the post-`create` duplicate-fingerprint mapping
in step 6 are the only new error paths. Everything else is a re-use of
existing primitives.

**New primitives introduced by this slice** (none of these exist
today; the implementation order in § 13 covers them):

- `VaultError::UnsupportedFormat { reason: &'static str }` — new
  variant on the existing `VaultError` enum.
- `VaultService::import_ssh_identity(pem, name) -> Result<GeneratedSshIdentity, VaultError>`
  — new method (§ 5.2).
- `ImportSshIdentityRequest` DTO with the manual `Debug` impl shown
  in § 4.2.
- `write_ssh_identity_create_audit` route-private helper that
  appends one `ssh_identity_created` audit row with the payload
  contract from § 7.3 — sketched in § 7.2.
- New error mapping `ApiError::Conflict { entity: "ssh_identity",
  reason: Some("duplicate_fingerprint") }` for the
  `UNIQUE (owner_id, fingerprint_sha256)` violation classifier
  (reuses the existing `Conflict` variant; the `"duplicate_fingerprint"`
  reason string is new).

### 5.1 Mapping vault errors to ApiError

```
VaultError::UnsupportedFormat { reason: "encrypted" }
  → 400 invalid_input  message="unsupported_key_format encrypted"
VaultError::UnsupportedFormat { reason: "malformed" }
  → 400 invalid_input  message="unsupported_key_format malformed"
VaultError::UnsupportedFormat { reason: "not_a_private_key" }
  → 400 invalid_input  message="unsupported_key_format not_a_private_key"
VaultError::UnsupportedKeyType(tag)
  → 400 invalid_input  message="unsupported key_type \"<tag>\""
                       (mirrors the generate path's existing message)
VaultError::Encrypt
  → 500 internal       (effectively unreachable; treat as bug)
```

The `reason` enum is closed and small — the formatter never inserts
the parser's text. The pre-validator's PEM-header check already
catches "this isn't even a PEM" before we ever call `ssh-key`, so a
`malformed` reason at the vault layer specifically means "valid PEM
envelope, invalid openssh-key-v1 body."

### 5.2 The `import_ssh_identity` vault method

New method on `VaultService`, alongside the existing
`generate_ssh_identity` and `decrypt_private_key`:

```rust
pub fn import_ssh_identity(
    &self,
    pem: &[u8],          // ASCII OpenSSH PEM, validated by the DTO layer.
    name: &str,          // Trimmed identity name; used as the OpenSSH comment.
) -> Result<GeneratedSshIdentity, VaultError>
```

- Returns the existing `GeneratedSshIdentity` so the API handler does
  not branch on "generated vs imported" for the persistence call —
  the DB layer sees the same shape either way.
- The trailing comment is set to `name` for parity with the generate
  path. Operators expect "the name in RelayTerm matches the comment
  on the `authorized_keys` line."
- Drops the original `pem` reference at function return; the only
  durable form is `encrypted_private_key`.
- Tests in the existing pattern (`vault::identity::tests`): a
  round-trip Ed25519 import equals fingerprint of the original public
  key; an RSA OpenSSH PEM is rejected with `UnsupportedKeyType("rsa")`;
  an encrypted OpenSSH PEM is rejected with `UnsupportedFormat { reason:
  "encrypted" }`; a garbage byte string is rejected with
  `UnsupportedFormat { reason: "malformed" }`; the function's `Debug`
  output never echoes the PEM bytes.

## 6. Vault behavior — what stays the same

- **Envelope is unchanged.** The on-disk blob is exactly the existing
  `RTV1` + `version 0x01` (XChaCha20-Poly1305 / random nonce) shape.
  An imported key is indistinguishable from a generated key at rest.
- **`encrypted_private_key` column** stores exactly the same opaque
  bytes the generator already produces.
- **Decryption path is unchanged.** A future SSH-session task that
  reads the row to sign a handshake calls
  `VaultService::decrypt_private_key(blob)` and gets a
  `Zeroizing<Vec<u8>>` of OpenSSH PEM bytes. Whether the row arrived
  via the generate or the import route is invisible to that caller.
- **Master-key requirement is unchanged.** Import returns
  `503 service_unavailable` when `state.vault` is `None` — same
  message as the generate route ("backend vault is not configured").

## 7. Audit behavior

### 7.1 Kind: reuse `ssh_identity_created`

The existing `SshIdentityCreated` audit kind already exists in the
`audit_events_kind_chk` CHECK migration and in the `AuditEventKind`
enum but is **not currently emitted** by any route (see SPEC.md →
"Audit-event expectations"). Both this slice and a future "emit on
generate" slice can use it without a new migration.

Adding a distinct `SshIdentityImported` kind would require a paired
CHECK-constraint migration and a paired enum variant — overhead for a
discriminator that fits cleanly inside the payload.

### 7.2 Helper: `write_ssh_identity_create_audit` (new)

A route-private helper, **mirror of the existing
`write_ssh_identity_delete_audit`** in
`crates/relayterm-api/src/routes/v1/ssh_identities.rs`:

```rust
async fn write_ssh_identity_create_audit(
    state: &AppState,
    actor_id: UserId,
    identity: &SshIdentity,
    source: AuditSource,           // new tiny enum: Generated | Imported
) -> Result<(), ApiError> {
    let payload = json!({
        "ssh_identity_id": identity.id,
        "name": identity.name.as_str(),
        "key_type": identity.key_type,
        "fingerprint_sha256": identity.fingerprint_sha256,
        "created_at": identity.created_at,
        "source": source.as_str(),  // "generated" | "imported"
    });
    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(actor_id),
            kind: AuditEventKind::SshIdentityCreated,
            payload,
            remote_addr: None,
        })
        .await?;
    Ok(())
}
```

- **Build field-by-field.** Never `serde_json::to_value(domain_struct)`;
  the `SshIdentity` domain type carries `encrypted_private_key`
  bytes and `serde`'s derived impl would serialize them. The helper
  reads only the public columns.
- `AuditSource` is a tiny crate-private enum with two variants and
  `as_str()`. It does not need to live in `relayterm-core`.
- **Failure policy** is fail-closed: a failed audit insert surfaces
  as `RepositoryError → ApiError::Internal`. Both the generate and
  the import route emit through this helper, so the audit posture
  is uniform across the two creation paths.

### 7.3 Payload contract (public metadata only)

```jsonc
{
  "ssh_identity_id": "…uuid…",
  "name": "homelab-admin",
  "key_type": "ed25519",
  "fingerprint_sha256": "SHA256:…",
  "created_at": "2026-05-12T…Z",
  "source": "imported"     // discriminator; "generated" on the future generate-route emission
}
```

- Build field-by-field, mirror `write_ssh_identity_delete_audit`. No
  `serde_json::to_value(domain_struct)`. No raw russh / parser / DB
  error text. No `encrypted_private_key`. No `public_key` bytes. No
  passphrase (the variable does not exist in v1 anyway). No client
  user-agent / `client_info` blob.
- The `source` field lets the operator distinguish imports from
  generations in the `recent_for_actor` feed without exposing any
  additional metadata.
- Failure policy: **fail-closed.** Mirror the server-profile
  lifecycle audit. A failed audit insert surfaces as `RepositoryError
  → ApiError::Internal`; the route returns BEFORE the row is created
  has any user-visible side effect — actually, in this design the
  audit append happens AFTER the DB insert (mirroring the generate
  path), so a failed audit insert leaves the DB row in place. That is
  intentional and matches the precedent: `ssh_identity_deleted`
  audits BEFORE delete (the row goes away); `ssh_identity_created`
  audits AFTER create (the row already exists and a retry can append
  a duplicate row only if the request is retried with the same body —
  which the unique-fingerprint constraint refuses with a clean 409).

### 7.3 Sentinel-based redaction tests

Add to `crates/relayterm-api/tests/api.rs`:

- A test analogous to existing `AUDIT_FORBIDDEN_SUBSTRINGS` coverage
  that fires an import with a sentinel PEM body (and a sentinel
  comment), then asserts no audit row's payload column contains any
  of: the sentinel PEM body bytes, the sentinel comment, the literal
  string `-----BEGIN OPENSSH PRIVATE KEY-----`, the literal string
  `encrypted_private_key`, the literal string `private_key`.

## 8. Error semantics

| Symptom | Wire response | Notes |
|---|---|---|
| `name` missing / empty / whitespace / control / too long | `400 invalid_input` (reuses generate-path messages byte-for-byte) | `validate_identity_name` |
| `private_key_openssh` non-ASCII | `400 invalid_input` `"private_key_openssh must be ASCII"` | DTO validator |
| `private_key_openssh` > 8 KiB | `400 invalid_input` `"private_key_openssh must not exceed 8 KiB"` | DTO validator |
| `private_key_openssh` missing `-----BEGIN OPENSSH PRIVATE KEY-----` | `400 invalid_input` `"private_key_openssh is missing OpenSSH PEM header"` | DTO validator |
| valid PEM envelope, malformed openssh-key-v1 body | `400 invalid_input` `"unsupported_key_format malformed"` | vault `from_openssh` failure mapped without echoing parser text |
| valid PEM, encrypted (passphrase-protected) | `400 invalid_input` `"unsupported_key_format encrypted"` | `PrivateKey::is_encrypted()` |
| valid PEM, valid private key, algorithm != Ed25519 | `400 invalid_input` `"unsupported key_type \"<tag>\""` | shares the message shape with the generate route's existing `parse_supported_key_type` |
| valid Ed25519 private key, fingerprint already owned by caller | `409 conflict { entity: "ssh_identity", reason: "duplicate_fingerprint" }` | the schema's `UNIQUE (owner_id, fingerprint_sha256)` index is the source of truth; the route classifies the FK violation before the audit append |
| vault disabled | `503 service_unavailable` | same body as the generate route (`"vault is disabled; backend-generated SSH identities require a master key"`) — operator UX rationale matches |
| not authenticated | `401` (`AuthenticatedUser` extractor) | unchanged from the rest of the protected surface |
| CSRF / Origin mismatch | `403 csrf_origin_mismatch` (`CsrfGuard`) | unchanged |

**Never** mapped:

- The raw `ssh-key` error text into any wire field. The four
  `VaultError::UnsupportedFormat` reasons (`encrypted`, `malformed`,
  `not_a_private_key`, plus the keep-the-list-closed mantra) are
  enumerated in the vault crate; the API layer is a pure function of
  the discriminant.
- The supplied PEM bytes into any wire field, log line, or audit row.
- Any byte from the request body into the operator-side `warn!`
  beyond the discriminant tag.

## 9. Frontend UX

### 9.1 Identities view layout

Add a sibling panel to the existing "Generate SSH identity" surface on
`apps/web/src/lib/app/views/IdentitiesView.svelte`:

- View header gains a second action button: **"Import SSH identity"**
  alongside the existing **"Generate SSH identity"**. The two panels
  are mutually exclusive — opening one closes the other.
- The import panel has:
  - A `name` input (same validation, same `MAX_IDENTITY_NAME_LEN`
    constant).
  - A `private_key_openssh` `<textarea>` (rows ~10; monospace;
    `autocomplete="off"`, `autocorrect="off"`, `autocapitalize="off"`,
    `spellcheck="false"`).
  - A "Cancel" button (disabled while submitting).
  - A submit button labelled "Import SSH identity" (disabled while
    submitting, while `name` is empty after trim, or while the
    textarea is empty).
- **No file picker in v1.** Paste-only is the deliberately narrower
  attack surface — no `<input type="file">` means no accidental
  filesystem read, no `FileReader` race, no "did the browser cache
  the picked file in some history surface" question. The textarea is
  the single ingress point.
- **No passphrase field in v1.**

### 9.2 Warning copy (load-bearing)

The panel renders an intro paragraph above the form, identical
sentence for sentence in copy review:

> The private key you paste here is sent to your RelayTerm server over
> HTTPS, parsed once on the backend, encrypted into the server-side
> vault using the operator-configured master key, and never returned
> to the browser. It is not stored in localStorage, sessionStorage, or
> any browser cache. The textarea is cleared on success and on every
> failure.

And below the textarea, in a smaller line:

> Only OpenSSH-format Ed25519 private keys without a passphrase are
> supported in this release. Encrypted keys and other algorithms will
> be added in a later slice.

### 9.3 Field-clearing discipline

- On successful import, the `name` and `private_key_openssh` script
  variables are reassigned to `""` **before** the success card is
  shown.
- On any failure (validation / HTTP / transport / malformed), the
  `private_key_openssh` variable is cleared to `""`. The `name` is
  preserved (operator probably wants to retry with the same label).
- On panel close, both variables are cleared.
- The textarea is **never** bound to a value that survives a
  component unmount. No `bind:value` to a store. No `data-*`
  attribute that captures the value. No `title=` / `aria-label`
  derivation from the value.
- Pinned by a frontend test in the style of
  `inventoryMutationsApi.test.ts`: simulate paste → submit → success
  / failure, then assert `pendingImportPrivateKey === ""` and that
  `JSON.stringify(componentState)` contains none of the sentinel
  bytes pasted in.

### 9.4 Helper additions

In `apps/web/src/lib/api/sshIdentities.ts`, alongside the existing
`createSshIdentity`:

```ts
export interface ImportSshIdentityRequest {
  name: string;
  private_key_openssh: string;
}

export type ImportRequestInvalidReason =
  | "missing_name"
  | "name_has_surrounding_whitespace"
  | "name_too_long"
  | "name_has_control_chars"
  | "missing_private_key"
  | "private_key_not_ascii"
  | "private_key_too_long"
  | "private_key_missing_pem_header";

export type ImportSshIdentityError =
  | { kind: "validation"; reason: ImportRequestInvalidReason }
  | {
      kind: "http";
      status: number;
      code: string;
      message: string;
      reason: "duplicate_fingerprint" | "unsupported_format_encrypted" |
              "unsupported_format_malformed" | "unsupported_key_type" | null;
    }
  | { kind: "transport"; message: string }
  | { kind: "malformed_response" };

export async function importSshIdentity(
  req: ImportSshIdentityRequest,
  options?: ImportSshIdentityOptions,
): Promise<{ ok: true; identity: SshIdentity } | { ok: false; error: ImportSshIdentityError }>;

export function describeImportSshIdentityError(err: ImportSshIdentityError): string;
```

- Validates the same shape rules locally to refuse a bad submit
  without a wire round-trip; backend remains authoritative.
- The `reason` field on the `http` variant is derived from the
  envelope `message` string by exact-match (mirror of the existing
  `deleteSshIdentity` helper's `"ssh_identity referenced"` mapping).
- `describeImportSshIdentityError` follows the same rule as every
  other formatter on the page: a function of `kind` + `status` +
  `code` (+ `reason` enum) only. Never the wire `message`, never the
  thrown `Error.message`. Sentinel tests pin this.

### 9.5 Mobile usability

- The textarea uses `inputmode="text"` and disables the platform
  spell-checker / autocorrect (`autocapitalize="off"`,
  `spellcheck="false"`, `autocorrect="off"`).
- On narrow viewports the panel renders the textarea full-width
  before the submit row, exactly like the generate panel's name
  input — no horizontal scroll, no font-size shrink that would
  trigger iOS zoom-on-focus.
- "Import SSH identity" is a long label; the button label collapses
  to "Import" on narrow viewports (same pattern as
  `MobileIdentifierInputs`).

## 10. Out of scope for v1 (deferred to later slices)

| Item | Why deferred |
|---|---|
| Passphrase-protected OpenSSH keys | Requires `ssh-key` `encryption` feature + cipher backend + a passphrase channel. Separate security-review surface. v1.1. |
| RSA / ECDSA import | Requires `ssh-key` `rsa` / `p256` / `p384` / `p521` features (larger transitive dep, broader audit). Operators with existing RSA keys are best served generating fresh Ed25519. |
| PEM PKCS#1 / PKCS#8 import | Different parser path; not an SSH-native format. v1.x if demand emerges. |
| Putty `.ppk` | Different parser; needs a new dep. v1.x. |
| File picker (browse for a key file) | Doubles the ingress surface for marginal UX gain. Paste is the universal path; file pick lands later behind the same `POST /api/v1/ssh-identities/import` endpoint. |
| Bulk import (multiple PEMs in one call) | Quota / audit / error-aggregation design is non-trivial. No demand in single-user v1. |
| Hardware-backed / FIDO / U2F / smart-card SSH keys | The backend would have to proxy a hardware-bound signing flow (no plaintext private key exists to import). Architecturally different from BYOK; deferred. |
| SSH certificates (CA-signed) | A user-CA model is a separate, deliberate feature. Out of scope. |
| Agent forwarding | Architecturally incompatible with the "private keys live in the backend vault, never in the browser, never in a remote agent" model. Out of scope for v1. |
| Long-term key storage in the browser | Explicitly forbidden — re-affirmed by the existing redaction discipline. |
| Admin / cross-user import | v1 is single-user. The route uses `AuthenticatedUser::user_id()`; the row is owner-scoped. An admin import path is a future, deliberate slice. |
| Key rotation workflow (re-key a profile) | Architecturally adjacent (delete-old + import-new + re-bind), but enough scope for its own design doc. |
| `ssh-copy-id` / password-bootstrap automation | Independent feature. Not "import." See SPEC.md "Out of scope (v1)" — the two are deliberately not conflated. |

## 11. Security and redaction tests

Tests required for the implementation slice (read this section
alongside `docs/agent/redaction-rules.md` §§ 1, 3, 7, 8).

### 11.1 Backend (Rust)

- `crates/relayterm-vault/src/identity.rs::tests`:
  - `import_round_trips_ed25519` — generate a fresh Ed25519 via
    `ssh-key`, serialize to OpenSSH PEM, hand to
    `VaultService::import_ssh_identity`, assert the returned
    `fingerprint_sha256` equals the original `PublicKey::fingerprint`.
  - `import_rejects_rsa` — feed an RSA OpenSSH PEM (test fixture; not
    committed if large — generate at test time using a fixed seed if
    `ssh-key`'s RSA test deps land; otherwise skip and rely on the
    API-layer test feeding a wire-shaped RSA PEM).
  - `import_rejects_encrypted` — feed an encrypted Ed25519 OpenSSH
    PEM, assert `VaultError::UnsupportedFormat { reason: "encrypted" }`.
  - `import_rejects_garbage` — feed `b"not a key"`, assert
    `VaultError::UnsupportedFormat { reason: "malformed" }`.
  - `import_returns_ciphertext_not_plaintext` — assert the returned
    `encrypted_private_key.as_bytes()` does not contain the literal
    PEM header bytes.
  - `import_debug_does_not_leak_pem` — `format!("{:?}", import_input)`
    contains no PEM bytes.
- `crates/relayterm-api/tests/api.rs`:
  - `import_ssh_identity_201_returns_public_metadata_only` — POST a
    valid Ed25519 PEM, assert 201, parse the response, assert no
    `encrypted_private_key` / `private_key` field on the JSON object.
  - `import_ssh_identity_duplicate_fingerprint_409` — POST the same
    PEM twice; second call returns
    `409 conflict { entity: "ssh_identity", reason: "duplicate_fingerprint" }`.
    Assert exactly one audit row was written across both calls
    (idempotency keystone, mirrors redaction-rules § 2).
  - `import_ssh_identity_audit_payload_redacted` — POST a PEM whose
    body contains a sentinel marker string; query `audit_events`;
    assert no row's `payload` JSON contains the sentinel marker, the
    literal `-----BEGIN OPENSSH PRIVATE KEY-----`, the literal
    `encrypted_private_key`, or the literal `private_key`. Use the
    existing `AUDIT_FORBIDDEN_SUBSTRINGS` helper.
  - `import_ssh_identity_encrypted_pem_400` — POST an encrypted
    Ed25519 PEM, assert 400 with `unsupported_key_format encrypted`.
  - `import_ssh_identity_rsa_400` — POST an RSA OpenSSH PEM, assert
    400 with `unsupported key_type "rsa"`.
  - `import_ssh_identity_csrf_origin_mismatch_403` — POST with a
    disallowed `Origin` AND a malformed JSON body, assert 403
    (rejection-before-body-parse — mirrors
    `bad_origin_rejects_before_body_parsing`).
  - `import_ssh_identity_unauthenticated_401` — POST with no cookie,
    assert 401.
  - `import_ssh_identity_oversized_body_400` — POST a 9 KiB body,
    assert 400 (the DTO size cap).
  - `import_ssh_identity_logs_do_not_contain_pem` — pin via a tracing
    subscriber capture (or by structural assertion on the log line
    template) that no logged line contains the sentinel PEM bytes
    across success / failure paths.

### 11.2 Frontend (TypeScript)

- `apps/web/tests/inventoryMutationsApi.test.ts` (extend):
  - `importSshIdentity` happy path — 201 → parsed object has no
    `private_key` / `encrypted_private_key` property; the formatted
    success line never mentions either substring.
  - `importSshIdentity` HTTP errors — for each of `400
    unsupported_key_format encrypted`, `400 unsupported_key_format
    malformed`, `400 unsupported key_type "rsa"`, `409 ssh_identity
    duplicate_fingerprint`, `401`, `403 csrf_origin_mismatch`, `503
    service_unavailable` — assert the formatter output is a function
    of kind/status/code only and never echoes the wire `message`.
  - `importSshIdentity` transport error — assert the formatter output
    is the static `"transport error"` string regardless of the thrown
    `Error.message`.
  - Sentinel redaction — feed a request body whose `private_key_openssh`
    contains `SECRET-SENTINEL`; on success, on malformed response, on
    HTTP error, and on transport error: assert
    `JSON.stringify({ result, errorObj })` does not contain
    `SECRET-SENTINEL`.
- `apps/web/tests/<new>.test.ts` (component-level, optional first
  pass): the Identities view import panel clears
  `private_key_openssh` to `""` on success AND on every failure
  branch, and no `localStorage` / `sessionStorage` key is written
  during the flow (`expect(window.localStorage.length).toBe(0)`
  before/after).

## 12. Staging smoke plan

Mirrors `docs/deployment/vps-staging-smoke.md` "Inventory management
mutations" entry shape.

**Prereqs:** staging stack up; throwaway SSH key on the operator's
workstation only; production / personal keys explicitly excluded.

```sh
# Generate a throwaway key on the operator workstation, NOT in CI:
ssh-keygen -t ed25519 -N "" -C "relayterm-staging-smoke" -f /tmp/rt-smoke-ed25519
# /tmp/rt-smoke-ed25519     ← private (will be imported then revoked)
# /tmp/rt-smoke-ed25519.pub ← public  (will go on a throwaway target host)
```

Smoke procedure:

1. Auth-bootstrap (existing flow) → login → confirm cookie + CSRF
   posture.
2. **Import** the throwaway PEM:
   - Browser path: paste PEM into the Identities → Import panel,
     submit, confirm the success card shows public metadata only and
     the textarea is empty after.
   - API path: write the request body to a temp file via `jq`
     (POSIX-portable; works in `bash`, `fish`, and the staging
     runner's `sh`), then POST it with `curl`:

     ```sh
     jq -n --arg name smoke --rawfile pem /tmp/rt-smoke-ed25519 \
       '{ name: $name, private_key_openssh: $pem }' \
       > /tmp/rt-smoke-import.json
     curl -fSs -X POST \
       -b cookies.txt -H 'Origin: <allowed-origin>' \
       -H 'Content-Type: application/json' \
       --data-binary @/tmp/rt-smoke-import.json \
       https://<staging>/api/v1/ssh-identities/import \
       > /tmp/rt-smoke-import.resp
     rm /tmp/rt-smoke-import.json
     ```

     Assert `201`, assert `/tmp/rt-smoke-import.resp` contains no
     `encrypted_private_key` / `private_key` substring (`grep -F`),
     then `rm /tmp/rt-smoke-import.resp`.
3. Inspect the audit feed (`/api/v1/auth/audit-events` or the SQL
   level if the route is not yet user-facing for create events) —
   exactly one `ssh_identity_created` row with `source: "imported"`
   and a payload containing only the documented public fields.
4. Inspect the operator-side logs (`docker compose logs backend`) and
   the audit table — assert NEITHER contains any byte from the
   pasted PEM, the literal `-----BEGIN OPENSSH PRIVATE KEY-----`, the
   word `encrypted_private_key`, or the word `private_key`.
5. Create a throwaway target `host` (a sacrificial VM or container
   reachable only by the operator) → create a `server_profile` that
   binds the throwaway host to the imported identity.
6. Run host-key preflight + trust on the new profile.
7. Run auth-check on the new profile against the throwaway target
   (the throwaway target must have the public key half installed at
   `~/.ssh/authorized_keys` — that part is manual, since v1 does NOT
   include `ssh-copy-id`).
8. Launch a terminal against the throwaway profile (existing flow);
   confirm the SSH session works exactly as a generated-identity
   session would. This is the load-bearing parity check: an imported
   key is byte-for-byte indistinguishable downstream.
9. **Negative paths** — repeat the import with:
   - The same PEM (expect `409 duplicate_fingerprint`).
   - An encrypted Ed25519 PEM (expect `400 unsupported_key_format
     encrypted`).
   - An RSA PEM (expect `400 unsupported key_type "rsa"`).
   - A garbage body (expect `400`).
10. Delete the `server_profile`. Delete the imported `ssh_identity`
    (the existing DELETE route, refuses while a profile references
    it — verified by the order above).
11. Inspect audit one more time → confirm a single
    `ssh_identity_deleted` row.
12. **Destroy the throwaway key on the operator workstation** —
    `shred -u /tmp/rt-smoke-ed25519 /tmp/rt-smoke-ed25519.pub` (or
    the OS-equivalent secure-delete).
13. Record the smoke in `docs/deployment/vps-staging-smoke.md`
    following the existing "Inventory management mutations" entry's
    one-paragraph format. Note the throwaway-key discipline and the
    audit-redaction sweep result.

**No production keys.** The entire procedure runs against a key
generated for the smoke and destroyed at the end. If any operator is
tempted to "just paste my real ~/.ssh/id_ed25519 because it would be
faster," stop and generate a throwaway. This is a load-bearing rule —
the smoke entry in `docs/deployment/vps-staging-smoke.md` should
restate it.

## 13. Implementation order (when the slice is picked up)

1. **Vault** — add `import_ssh_identity` + the `UnsupportedFormat`
   error variants + unit tests. No API surface yet.
2. **DTO** — add `ImportSshIdentityRequest` + `validate()` +
   per-rule tests in `crates/relayterm-api/src/dto/ssh_identity.rs`.
3. **Route** — add `POST /api/v1/ssh-identities/import` with the
   handler in § 5. Wire-level integration tests as in § 11.1.
4. **Audit emit** — both the new import route AND (in the same
   slice) the existing generate route gain a `write_ssh_identity_create_audit`
   call. The generate path's audit gain is incidental but consistent
   ("creation should audit" is a property we'd want even if import
   weren't landing). The `source` discriminator distinguishes them
   in the payload.
5. **TS helper** — `importSshIdentity` + validator + formatter in
   `apps/web/src/lib/api/sshIdentities.ts`. Unit tests as in § 11.2.
6. **UI** — Import panel on `IdentitiesView.svelte`. The existing
   "Generate" panel is unchanged; the two panels share a single
   "panel open" state machine and a single success-card area.
7. **Docs** — `SPEC.md` "Inventory lifecycle …" → swap the
   "private-key import" deferred line to "landed (see X)";
   `docs/spec/inventory.md` "Production SSH identity generation UI"
   → add a sibling "Production SSH identity import UI" section;
   `docs/agent/redaction-rules.md` § 1 / § 3 keep the existing rules
   without change (they already cover the new payload).
8. **Staging smoke** — run § 12; record in
   `docs/deployment/vps-staging-smoke.md`.

Each step is independently shippable except (3)/(4) (route + audit
land together so the route's CSRF/auth/idempotency tests can be
written against the final audit shape) and (5)/(6) (the TS helper
and the panel land together so the formatter has at least one real
caller).

## 14. Open questions

1. **Should the API accept `private_key_pem` in v1 as a second
   field?** _Recommendation:_ no. A single field keeps the wire
   shape simple; a `format: "openssh"` discriminator can be added
   later if/when PEM PKCS#8 import becomes a v1.x slice.
2. **Should the audit kind be `ssh_identity_imported` instead of
   `ssh_identity_created` + `source: "imported"`?** _Recommendation:_
   reuse `ssh_identity_created` + `source`. Avoids a new
   audit-CHECK migration; the discriminator lives in the payload
   where it can also serve a future generate-path emission.
3. **Should a non-Ed25519 valid OpenSSH key be a `400 invalid_input`
   (current proposal) or a `415 Unsupported Media Type`?**
   _Recommendation:_ `400 invalid_input` — matches the existing
   generate-path's `parse_supported_key_type` shape so clients see
   one canonical envelope.
4. **Should the textarea bind a `value` with cleared-on-success
   discipline, or should it be a `ref`-style read-once-on-submit
   input?** _Recommendation:_ bound value with explicit clear-on-
   success / clear-on-failure / clear-on-close — the test surface
   is more straightforward (`expect(state.private_key_openssh).toBe("")`).
   The cost of binding is the discipline to clear; the benefit is
   visible UI state during a multi-step form (paste → review →
   submit).
5. **v1.1 passphrase channel — request body field name?**
   _Recommendation:_ `passphrase: Option<String>`, treated as
   `Zeroizing<String>` immediately on entry. Never logged. Never
   echoed. Never returned. The forbidden-substring sentinel tests
   are extended to cover the passphrase value.
6. **Rate-limit / throttle on the import route?** Single-user v1
   does not need a per-route throttle beyond the existing
   `LoginThrottler` posture on auth. _Recommendation:_ defer; revisit
   if a multi-user slice ever lands.
7. **Should the import path automatically test the keypair (sign +
   verify) before accepting it?** `ssh-key`'s parse already
   validates structure; an explicit sign-then-verify before persist
   would catch a (highly unlikely) post-parse corruption that the
   library missed. _Recommendation:_ skip in v1 — the
   parse-and-re-serialize round-trip via `to_openssh(LineEnding::LF)`
   already exercises the key bytes; add it later only if a
   real-world bug demonstrates the need.

---

_Authored as a design doc on the `docs/private-key-import-design`
branch. No code changes accompany this commit._
