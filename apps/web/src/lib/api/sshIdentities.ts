/**
 * Frontend helper for `GET /api/v1/ssh-identities`.
 *
 * **Security-critical:** the {@link SshIdentity} type intentionally
 * does NOT declare `encrypted_private_key` or `private_key` fields.
 * The backend's `SshIdentityResponse` already drops `encrypted_private_key`
 * before serialization, but the parser here defends in depth: the
 * runtime DTO is built field-by-field from the typed interface, so a
 * server bug or future test fixture that accidentally includes the
 * field cannot smuggle it into the parsed object. Test sentinels in
 * `tests/inventoryApi.test.ts` pin this against future regressions.
 *
 * Read-only: list and get only. Generation, deletion, and key import
 * UI are future work.
 */

import {
  fetchJsonList,
  type LoadOptions,
  type LoadResult,
} from "./apiErrors.js";

/** Wire-stable algorithm tag mirroring `SshKeyType` on the backend. */
export type SshKeyType =
  | "ed25519"
  | "rsa"
  | "ecdsa_p256"
  | "ecdsa_p384"
  | "ecdsa_p521";

const KEY_TYPES: ReadonlySet<SshKeyType> = new Set([
  "ed25519",
  "rsa",
  "ecdsa_p256",
  "ecdsa_p384",
  "ecdsa_p521",
]);

/**
 * SSH identity public-metadata DTO.
 *
 * No private-key field is declared. Adding one would be a security
 * regression — the redaction-sentinel test pins the absence of any
 * `private_key`-suffixed property in the parsed object and in the
 * formatted preview/copy strings.
 */
export interface SshIdentity {
  id: string;
  name: string;
  key_type: SshKeyType;
  /** OpenSSH-format public key (e.g. `ssh-ed25519 AAAA...`). Safe
   * to render and copy. */
  public_key: string;
  /** SHA-256 fingerprint string (`SHA256:<base64>`). */
  fingerprint_sha256: string;
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp; absent when the identity has never been
   * used to authenticate against any host. */
  last_used_at: string | null;
}

export interface ListSshIdentitiesOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/ssh-identities`. */
  endpoint?: string;
}

export async function listSshIdentities(
  options: ListSshIdentitiesOptions = {},
): Promise<LoadResult<SshIdentity[]>> {
  const endpoint = options.endpoint ?? "/api/v1/ssh-identities";
  return fetchJsonList<SshIdentity>(endpoint, parseSshIdentity, options);
}

export function parseSshIdentity(raw: unknown): SshIdentity | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.name !== "string" ||
    typeof r.key_type !== "string" ||
    typeof r.public_key !== "string" ||
    typeof r.fingerprint_sha256 !== "string" ||
    typeof r.created_at !== "string"
  ) {
    return null;
  }
  if (!KEY_TYPES.has(r.key_type as SshKeyType)) {
    return null;
  }
  if (r.last_used_at !== null && typeof r.last_used_at !== "string") {
    return null;
  }
  // Construct field-by-field. A stray `encrypted_private_key` /
  // `private_key` on `r` cannot reach the returned object because no
  // path here copies it. This is also pinned by the redaction tests.
  return {
    id: r.id,
    name: r.name,
    key_type: r.key_type as SshKeyType,
    public_key: r.public_key,
    fingerprint_sha256: r.fingerprint_sha256,
    created_at: r.created_at,
    last_used_at: r.last_used_at,
  };
}

/**
 * One-line preview of a public key (algorithm + the first segment of
 * the base64 body) for table-row display. The full key is available
 * via {@link SshIdentity.public_key}; this helper exists so the table
 * does not wrap a 400-char string when rendered.
 *
 * Always operates on the public key only — never on private material.
 * If a future revision is tempted to include comment/fingerprint
 * detail, keep the function pure and avoid concatenating identifiers
 * from anywhere else on the row.
 */
export function publicKeyPreview(publicKey: string, max = 24): string {
  const trimmed = publicKey.trim();
  if (trimmed.length === 0) return "";
  const parts = trimmed.split(/\s+/);
  if (parts.length < 2) {
    return trimmed.length <= max ? trimmed : `${trimmed.slice(0, max)}…`;
  }
  const [algo, body] = parts;
  if (body.length <= max) return `${algo} ${body}`;
  return `${algo} ${body.slice(0, max)}…`;
}
