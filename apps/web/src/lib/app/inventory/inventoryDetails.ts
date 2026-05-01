/**
 * Pure helpers for the production read-only inventory detail panels.
 *
 * The view components (`ServersView.svelte`, `IdentitiesView.svelte`)
 * keep the imperative load / select / close wiring; the field-level
 * shapes that get rendered into the panel sit here so vitest can pin
 * the contract without a Svelte runtime.
 *
 * Honesty rules (mirror SPEC.md "Production inventory read-only views"):
 *  - Detail panels show ONLY data the caller has already loaded into the
 *    list view. No new backend round-trips are introduced for detail.
 *  - Related-object summaries (profiles for a host, host/identity for a
 *    profile) are joins over the supplied lists; an unresolved link is
 *    rendered honestly, never synthesised.
 *  - The panels do NOT imply terminal-readiness, host-key trust, or
 *    auth-check success from counts or links alone. Those facts live on
 *    the per-profile state stores that this module deliberately does
 *    NOT consume.
 *
 * Redaction posture (load-bearing):
 *  - {@link identityPublicDetail} accepts a typed {@link SshIdentity}
 *    and returns ONLY public metadata. The underlying parser already
 *    rejects `private_key`/`encrypted_private_key` from the wire; this
 *    helper builds the detail object field-by-field, so a future test
 *    fixture that fakes a private-key field on the input cannot smuggle
 *    it into the returned object.
 *  - {@link publicKeyCopyValue} is the single point that yields the full
 *    OpenSSH public key for a deliberate copy action. The "preview" the
 *    detail card renders is computed via the existing
 *    `publicKeyPreview` helper from `lib/api/sshIdentities.ts` — it
 *    deliberately truncates, so the full key never reaches an incidental
 *    hover surface or `title=` attribute.
 *  - No helper here logs, throws, or formats raw response bodies.
 */

import type { Host } from "../../api/hosts.js";
import type {
  ProfileLinks,
  ServerProfile,
} from "../../api/serverProfiles.js";
import { resolveProfileLinks } from "../../api/serverProfiles.js";
import type { SshIdentity, SshKeyType } from "../../api/sshIdentities.js";

/**
 * Truncate a UUID-shaped identifier for compact display next to a
 * full-name label. The full id is always available on the underlying
 * object; this helper exists so the detail summary line does not wrap
 * a 36-char string when the operator only needs to disambiguate two
 * rows with the same name. Returns the raw string when shorter than
 * the requested prefix; never logs or hashes the id.
 */
export function shortId(id: string, prefixLen = 8): string {
  if (typeof id !== "string") return "";
  if (id.length <= prefixLen) return id;
  return `${id.slice(0, prefixLen)}…`;
}

/**
 * Fallback for UI fields that may legitimately be absent from the
 * loaded list data (e.g. `last_connected_at` on a profile that has
 * never connected). Returns the supplied value when it is a non-empty
 * string; otherwise the supplied placeholder. Pure, side-effect-free,
 * never falls back through a side channel.
 */
export function safeDisplayValue(
  value: string | null | undefined,
  placeholder = "—",
): string {
  if (typeof value !== "string") return placeholder;
  if (value.length === 0) return placeholder;
  return value;
}

/**
 * Count of profiles whose `host_id` matches the supplied host.
 *
 * Operates only on the profiles already loaded by the Servers view —
 * never triggers a new fetch. A stale snapshot is honest about being
 * stale: this is a "shown to me right now" number, not a live total.
 */
export function hostProfileCount(
  host: Host,
  profiles: readonly ServerProfile[],
): number {
  let n = 0;
  for (const p of profiles) {
    if (p.host_id === host.id) n += 1;
  }
  return n;
}

/**
 * Profile rows currently linked to the supplied host. Returns the
 * input order so the detail panel renders profiles in the same order
 * as the main list. Pure; does not mutate the input.
 */
export function relatedProfilesForHost(
  host: Host,
  profiles: readonly ServerProfile[],
): ServerProfile[] {
  return profiles.filter((p) => p.host_id === host.id);
}

/**
 * Public-metadata-only summary of an SSH identity, suitable for
 * embedding inside a profile detail panel. Builds the result
 * field-by-field — `private_key` / `encrypted_private_key` cannot
 * appear on the returned object.
 */
export interface IdentitySummary {
  id: string;
  name: string;
  key_type: SshKeyType;
  fingerprint_sha256: string;
}

export function identitySummary(identity: SshIdentity): IdentitySummary {
  return {
    id: identity.id,
    name: identity.name,
    key_type: identity.key_type,
    fingerprint_sha256: identity.fingerprint_sha256,
  };
}

/**
 * Detail-panel projection for a server profile. Includes the profile
 * itself, the resolved host link (or `null`), the resolved identity
 * summary (or `null`), and the effective-username + inheritance flags
 * already produced by {@link resolveProfileLinks}.
 *
 * The `identity` field is a redaction-safe summary (id + name +
 * key_type + fingerprint) — it deliberately does NOT include the full
 * public key or any private material. The detail panel embeds the
 * fingerprint inline; the full key is reachable from the SSH
 * Identities view, where the deliberate copy action lives.
 *
 * Honest about unresolvable links: when the linked host or identity is
 * not in the supplied list (deleted, foreign-owned, or out of scope),
 * the corresponding field is `null`. The panel renders this as "host
 * not in your inventory" / "identity metadata available in SSH
 * Identities view" — it does NOT synthesise a placeholder.
 */
export interface ProfileDetail {
  profile: ServerProfile;
  links: ProfileLinks;
  identity: IdentitySummary | null;
}

export function resolveProfileDetail(
  profile: ServerProfile,
  hosts: readonly Host[],
  identities: readonly SshIdentity[],
): ProfileDetail {
  const links = resolveProfileLinks(profile, hosts);
  const matched = identities.find((i) => i.id === profile.ssh_identity_id);
  const identity = matched ? identitySummary(matched) : null;
  return { profile, links, identity };
}

/**
 * Setup-readiness hint for a profile, derived ONLY from data the
 * Servers view already has in hand at the moment the detail panel
 * opens. The dashboard checklist already states the rule explicitly:
 * counts cannot prove "public key installed" / "host-key trusted" /
 * "auth-check passed". The detail panel mirrors that rule — it does
 * NOT inspect the per-profile host-key trust / auth-check stores; the
 * panels for those flows already render their own status next to the
 * row.
 *
 * The values returned here are advisory copy strings, not booleans
 * about the live SSH state.
 */
export interface ProfileReadinessHint {
  /** Whether the host link resolved against the loaded host list. */
  hostLinkResolved: boolean;
  /** Whether the SSH identity link resolved against the loaded
   * identities list. */
  identityLinkResolved: boolean;
  /** Single advisory line describing what the operator should still
   * verify before relying on this profile to reach a server. Never
   * implies success; never echoes a wire message. */
  advisory: string;
}

export function describeReadinessFromKnownState(
  detail: ProfileDetail,
): ProfileReadinessHint {
  const hostLinkResolved = detail.links.host !== null;
  const identityLinkResolved = detail.identity !== null;
  if (!hostLinkResolved && !identityLinkResolved) {
    return {
      hostLinkResolved,
      identityLinkResolved,
      advisory:
        "Host and SSH identity links cannot be resolved from your inventory. Reachability and auth are unverified.",
    };
  }
  if (!hostLinkResolved) {
    return {
      hostLinkResolved,
      identityLinkResolved,
      advisory:
        "Host link cannot be resolved from your inventory. Reachability and auth are unverified.",
    };
  }
  if (!identityLinkResolved) {
    return {
      hostLinkResolved,
      identityLinkResolved,
      advisory:
        "SSH identity link cannot be resolved from your inventory. Auth is unverified.",
    };
  }
  return {
    hostLinkResolved,
    identityLinkResolved,
    advisory:
      "Host metadata and SSH identity are linked. Host-key trust and auth-check still need to pass before launching a terminal.",
  };
}

/**
 * Detail-panel projection for an SSH identity. Built field-by-field
 * from typed {@link SshIdentity} input — `private_key` /
 * `encrypted_private_key` cannot appear on the returned object. Pinned
 * by the redaction-sentinel tests in `tests/inventoryDetails.test.ts`.
 *
 * `publicKeyPreview` is the truncated form intended for the in-card
 * summary so a 400-char key does not wrap. The full key is exposed
 * ONLY through {@link publicKeyCopyValue} (the deliberate copy
 * action). Splitting "preview" and "copy" keeps the full key out of
 * incidental hover/title surfaces.
 */
export interface IdentityPublicDetail {
  id: string;
  name: string;
  key_type: SshKeyType;
  fingerprint_sha256: string;
  publicKeyPreview: string;
  created_at: string;
  last_used_at: string | null;
}

export function identityPublicDetail(
  identity: SshIdentity,
  preview: (publicKey: string) => string,
): IdentityPublicDetail {
  return {
    id: identity.id,
    name: identity.name,
    key_type: identity.key_type,
    fingerprint_sha256: identity.fingerprint_sha256,
    publicKeyPreview: preview(identity.public_key),
    created_at: identity.created_at,
    last_used_at: identity.last_used_at,
  };
}

/**
 * The single helper that yields the FULL OpenSSH public key for a
 * deliberate copy action. The detail-panel preview is computed via
 * {@link identityPublicDetail}; the full string is produced here so
 * the test suite has a one-call surface to assert "no private-key
 * material ever appears in the copy value." Pure; never falls back.
 */
export function publicKeyCopyValue(identity: SshIdentity): string {
  return identity.public_key;
}
