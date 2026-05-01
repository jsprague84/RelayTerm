/**
 * Pure helpers for the production inventory search + filter controls.
 *
 * The Servers and Identities views own the imperative `$state` and the
 * input bindings; the matching, normalisation, and tag-collection logic
 * lives here so vitest can pin the contract without a Svelte runtime.
 *
 * Architectural rules (mirror SPEC.md "Production inventory client-side
 * search & filters"):
 *  - Filtering is in-memory only over already-loaded list data. No
 *    helper here calls fetch, no helper triggers a backend round-trip,
 *    no helper persists state to localStorage / URL / cookie.
 *  - Helpers do NOT mutate the input arrays. Each search/filter helper
 *    returns a new array; tag collection returns a freshly-built array.
 *  - Helpers do NOT log, throw, or format raw response bodies. The
 *    search inputs are user-typed UI text and are never written to a
 *    log surface.
 *
 * Redaction posture (load-bearing):
 *  - The identity-side helpers operate on the typed {@link SshIdentity}
 *    DTO, which already does NOT declare `private_key` /
 *    `encrypted_private_key`. The matching set built per identity here
 *    is constructed field-by-field, so a hostile fixture that smuggles
 *    a private-key field onto the input cannot reach the search index
 *    or any returned object. Pinned by the redaction-sentinel tests in
 *    `tests/inventoryFilters.test.ts`.
 *  - Profile/host helpers similarly enumerate fields explicitly. None
 *    of them dereference dynamic property names from user input.
 *  - Tag collection drops empty strings and dedupes — the deduped /
 *    sorted output is the only shape callers see.
 */

import type { Host } from "../../api/hosts.js";
import type { ServerProfile } from "../../api/serverProfiles.js";
import type { SshIdentity, SshKeyType } from "../../api/sshIdentities.js";

/**
 * Normalise a free-text search query for case-insensitive substring
 * matching. Trims surrounding whitespace, lowercases, and collapses
 * any internal run of whitespace to a single space so that
 * `"  Edge  Box  "` matches `"edge box"`.
 *
 * Returns the empty string when the input is `null`, `undefined`, or
 * non-string; the empty string is the canonical "no filter" sentinel
 * the search helpers below check.
 */
export function normalizeSearchText(input: unknown): string {
  if (typeof input !== "string") return "";
  const trimmed = input.trim();
  if (trimmed.length === 0) return "";
  return trimmed.toLowerCase().replace(/\s+/g, " ");
}

/**
 * Tokenize a normalised search query into one-or-more terms separated
 * by single spaces. Used so a multi-word query (`"prod east"`) matches
 * any haystack containing every term, regardless of order — the
 * single-space normalisation done by {@link normalizeSearchText} keeps
 * this trivial.
 */
function tokenize(normalised: string): string[] {
  if (normalised.length === 0) return [];
  return normalised.split(" ");
}

function matchesAllTokens(haystack: string, tokens: readonly string[]): boolean {
  if (tokens.length === 0) return true;
  const lower = haystack.toLowerCase();
  for (const token of tokens) {
    if (token.length === 0) continue;
    if (!lower.includes(token)) return false;
  }
  return true;
}

/**
 * Build the lower-cased haystack string for a host. Field-by-field so
 * a future addition to the {@link Host} DTO does not silently widen
 * the search surface. Includes display name, hostname, port (rendered
 * as decimal), and default username.
 */
function hostHaystack(host: Host): string {
  return `${host.display_name} ${host.hostname} ${host.port} ${host.default_username}`;
}

/**
 * Filter a list of hosts by a free-text query. Returns a NEW array; the
 * input is never mutated. An empty / whitespace-only query returns a
 * shallow copy of the input — the helper never returns the original
 * reference, so callers can safely treat the result as their own.
 */
export function filterHosts(
  hosts: readonly Host[],
  query: string,
): Host[] {
  const tokens = tokenize(normalizeSearchText(query));
  if (tokens.length === 0) return hosts.slice();
  return hosts.filter((host) => matchesAllTokens(hostHaystack(host), tokens));
}

/**
 * Resolve the effective username for a profile against the supplied
 * hosts list. Returns `null` when neither an override nor a resolvable
 * host default is available. Mirrors `resolveProfileLinks` but kept
 * private here so the search helper does not pull a heavier import.
 */
function effectiveUsernameFor(
  profile: ServerProfile,
  hosts: readonly Host[],
): string | null {
  if (profile.username_override !== null && profile.username_override.length > 0) {
    return profile.username_override;
  }
  const host = hosts.find((h) => h.id === profile.host_id);
  if (host) return host.default_username;
  return null;
}

/**
 * Build the lower-cased haystack string for a profile. Includes:
 *  - profile name + tags + (raw `username_override` when present)
 *  - effective username (override or host default) so a user typing
 *    only the inherited value still matches
 *  - linked host's display name + hostname (when resolvable)
 *  - linked identity's name + fingerprint + key type (when resolvable)
 *
 * Field-by-field, never `JSON.stringify(profile)` — extending the
 * search surface should be deliberate.
 *
 * Redaction posture: identity contributions are limited to `name`,
 * `fingerprint_sha256`, and `key_type`. The OpenSSH `public_key` body
 * is deliberately NOT in the haystack — it is large, base64-shaped, and
 * not useful for substring search; keeping it out of the index also
 * keeps the helper safe even if a renderer ever wired the haystack
 * into a debug surface.
 */
function profileHaystack(
  profile: ServerProfile,
  hosts: readonly Host[],
  identities: readonly SshIdentity[],
): string {
  const parts: string[] = [profile.name];
  for (const tag of profile.tags) parts.push(tag);
  if (profile.username_override !== null && profile.username_override.length > 0) {
    parts.push(profile.username_override);
  }
  const effective = effectiveUsernameFor(profile, hosts);
  if (effective !== null) parts.push(effective);
  const linkedHost = hosts.find((h) => h.id === profile.host_id);
  if (linkedHost) {
    parts.push(linkedHost.display_name);
    parts.push(linkedHost.hostname);
  }
  const linkedIdentity = identities.find(
    (i) => i.id === profile.ssh_identity_id,
  );
  if (linkedIdentity) {
    parts.push(linkedIdentity.name);
    parts.push(linkedIdentity.fingerprint_sha256);
    parts.push(linkedIdentity.key_type);
  }
  return parts.join(" ");
}

/**
 * Profile-side filter shape. Each field is independently optional so
 * the UI can compose search + tag without combinatorially exploding
 * named overloads.
 *
 * - `query` — free-text search; empty string disables it.
 * - `tag` — exact tag match against `profile.tags`; `null` / empty
 *   string disables it.
 * - `linkState` — restrict to profiles whose linked host / identity is
 *   resolvable / unresolvable against the supplied lists. `"any"`
 *   (the default) disables it. NOTE: this axis is wired into the
 *   helper but the Servers view does NOT currently expose a control
 *   for it. It is ready infrastructure for a future "show only
 *   profiles missing a linked host/identity" toggle; SPEC.md flags
 *   that as future work for this slice.
 */
export type ProfileLinkState = "any" | "missing_host" | "missing_identity";

export interface ProfileFilters {
  query?: string;
  tag?: string | null;
  linkState?: ProfileLinkState;
}

/**
 * Filter a list of server profiles by the supplied {@link ProfileFilters}.
 *
 * The filter is in-memory only — it operates over the already-loaded
 * lists supplied by the caller and never triggers a backend round-trip.
 * Returns a NEW array; the input is never mutated. With every filter
 * field empty / disabled, the helper returns a shallow copy of the
 * profiles input so callers can rely on result-array ownership.
 */
export function filterProfiles(
  profiles: readonly ServerProfile[],
  hosts: readonly Host[],
  identities: readonly SshIdentity[],
  filters: ProfileFilters = {},
): ServerProfile[] {
  const tokens = tokenize(normalizeSearchText(filters.query ?? ""));
  const tag =
    typeof filters.tag === "string" && filters.tag.length > 0
      ? filters.tag
      : null;
  const linkState: ProfileLinkState = filters.linkState ?? "any";
  // Symmetric with filterHosts / filterIdentities: a no-op filter
  // returns a fresh shallow copy so callers can rely on owning the
  // result array.
  if (tokens.length === 0 && tag === null && linkState === "any") {
    return profiles.slice();
  }
  return profiles.filter((profile) => {
    if (tag !== null && !profile.tags.includes(tag)) return false;
    if (linkState === "missing_host") {
      if (hosts.some((h) => h.id === profile.host_id)) return false;
    } else if (linkState === "missing_identity") {
      if (identities.some((i) => i.id === profile.ssh_identity_id)) return false;
    }
    if (
      tokens.length > 0 &&
      !matchesAllTokens(profileHaystack(profile, hosts, identities), tokens)
    ) {
      return false;
    }
    return true;
  });
}

/**
 * Identity-side filter shape.
 *
 * - `query` — free-text search over name, fingerprint, and key type.
 *   The OpenSSH `public_key` body is deliberately NOT searchable —
 *   substring matching against a 400-char base64 string is rarely
 *   useful and would invite a future preview surface that echoes the
 *   matched fragment back into a UI label. Identity public keys reach
 *   the DOM through the deliberate detail panel and copy action only.
 * - `keyType` — exact match against {@link SshIdentity.key_type};
 *   `null` / empty disables it.
 */
export interface IdentityFilters {
  query?: string;
  keyType?: SshKeyType | null;
}

function identityHaystack(identity: SshIdentity): string {
  return `${identity.name} ${identity.fingerprint_sha256} ${identity.key_type}`;
}

/**
 * Filter a list of SSH identities by the supplied
 * {@link IdentityFilters}. Returns a NEW array; the input is never
 * mutated. With every filter field empty / disabled, the helper returns
 * a shallow copy of the identities input.
 *
 * Redaction posture: the matching haystack does NOT include the
 * `public_key` field. A stray `private_key` / `encrypted_private_key`
 * field smuggled onto an input object cannot reach the haystack,
 * because the haystack is built field-by-field from typed properties.
 */
export function filterIdentities(
  identities: readonly SshIdentity[],
  filters: IdentityFilters = {},
): SshIdentity[] {
  const tokens = tokenize(normalizeSearchText(filters.query ?? ""));
  const keyType =
    typeof filters.keyType === "string" && filters.keyType.length > 0
      ? filters.keyType
      : null;
  // Symmetric with filterHosts / filterProfiles: a no-op filter
  // returns a fresh shallow copy.
  if (tokens.length === 0 && keyType === null) {
    return identities.slice();
  }
  return identities.filter((identity) => {
    if (keyType !== null && identity.key_type !== keyType) return false;
    if (
      tokens.length > 0 &&
      !matchesAllTokens(identityHaystack(identity), tokens)
    ) {
      return false;
    }
    return true;
  });
}

/**
 * Collect the set of unique tags currently in use across the supplied
 * profiles list, returned in case-insensitive ascending order. Empty
 * tags are dropped; the original tag string is preserved (so a UI that
 * displays the tag verbatim does not lose case). Pure; never mutates
 * the input.
 *
 * The tag-filter dropdown reads from this — sorted output keeps the
 * dropdown stable across renders even though the underlying profile
 * list is already in arbitrary order.
 */
export function collectProfileTags(
  profiles: readonly ServerProfile[],
): string[] {
  const seen = new Map<string, string>();
  for (const profile of profiles) {
    for (const tag of profile.tags) {
      if (typeof tag !== "string" || tag.length === 0) continue;
      const key = tag.toLowerCase();
      if (!seen.has(key)) seen.set(key, tag);
    }
  }
  return Array.from(seen.values()).sort((a, b) =>
    a.toLowerCase().localeCompare(b.toLowerCase()),
  );
}

/**
 * Format a "Showing X of Y <noun>" string for an inventory result
 * summary. Pure; the singular and plural noun forms are supplied by
 * the caller so irregular plurals (e.g. `identity` / `identities`) are
 * not mangled. `plural` defaults to `${singular}s` for the regular
 * case (`host` / `hosts`).
 *
 * When the visible count equals the total count, a shorter
 * "Y <noun>" form is returned so the operator does not see a noisy
 * "Showing 3 of 3 hosts" when no filter is active.
 */
export function countFilteredResults(
  visible: number,
  total: number,
  singular: string,
  plural: string = `${singular}s`,
): string {
  const noun = total === 1 ? singular : plural;
  if (visible === total) {
    return `${total} ${noun}`;
  }
  // Use the total-aware noun in the "Showing X of Y" form so reading
  // "Showing 1 of 3 hosts" stays grammatical — the field is still
  // plural even when only one row is visible.
  return `Showing ${visible} of ${total} ${noun}`;
}
