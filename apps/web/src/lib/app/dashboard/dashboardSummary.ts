/**
 * Pure helpers for the production Dashboard view.
 *
 * The view component (`DashboardView.svelte`) keeps the imperative load /
 * navigate / refresh wiring; everything with a stable contract — count
 * aggregation, session-status breakdown, checklist derivation, copy
 * strings, and the navigation target table — sits here so vitest can
 * pin the rules without a Svelte runtime.
 *
 * Honesty rules re-asserted (mirrors SPEC.md "Production dashboard
 * summary"):
 *  - Inventory counts collapse a {@link LoadResult} to a small union;
 *    a failed load surfaces as `unavailable`, NOT as zero. Fake zeros
 *    would lie about the operator's inventory.
 *  - The checklist infers ONLY what counts can prove: identity exists,
 *    host exists, profile exists, terminal session has been launched.
 *    It does NOT infer "public key installed", "host-key trusted",
 *    or "auth-check passed" — those need per-row state the dashboard
 *    cannot currently see, so the checklist marks them `manual` with
 *    honest copy.
 *  - The checklist does NOT imply terminal-launch readiness from
 *    counts alone. The "launch terminal" step is `complete` only if
 *    a session has actually been launched.
 *  - The redaction posture from the underlying API helpers is
 *    preserved: this module never copies fields off raw wire bodies,
 *    never declares `private_key` / `encrypted_private_key` /
 *    `session_output` / `access_token`, and never echoes wire detail
 *    in a formatted summary.
 */

import type { LoadResult } from "../../api/apiErrors.js";
import type { Host } from "../../api/hosts.js";
import type { ServerProfile } from "../../api/serverProfiles.js";
import type { SshIdentity } from "../../api/sshIdentities.js";
import type {
  TerminalSession,
  TerminalSessionStatus,
} from "../../api/terminalSessions.js";
import type { AppRoutePath } from "../routing.js";
import type { AppViewId } from "../navigation.js";

/**
 * Card-level state for an inventory tile. `loading` is the pre-fetch
 * placeholder; `unavailable` is a load failure. The tile renders an
 * em-dash for both non-`ready` cases — operator triage belongs in the
 * per-resource view, not here.
 */
export type CardState =
  | { kind: "loading" }
  | { kind: "ready"; value: number }
  | { kind: "unavailable" };

export interface InventoryCounts {
  hosts: CardState;
  profiles: CardState;
  identities: CardState;
  sessions: CardState;
}

export interface SessionStatusCounts {
  starting: number;
  active: number;
  detached: number;
  closed: number;
}

export type SessionStatusBreakdown =
  | { kind: "ready"; counts: SessionStatusCounts; total: number }
  | { kind: "unavailable" }
  | { kind: "loading" };

/**
 * Setup-checklist step state.
 *  - `complete`   — count-inferable AND the count is > 0.
 *  - `incomplete` — count-inferable AND the count is 0.
 *  - `manual`     — not safely inferable from current API data; the
 *                   operator verifies the step from the per-resource
 *                   view. Examples: "copy public key to server",
 *                   "host-key trust", "auth-check".
 *  - `unknown`    — required input is still loading or the load
 *                   failed. Honest neutral state — never coerce to
 *                   `complete` or `incomplete`.
 */
export type ChecklistStepStatus =
  | "complete"
  | "incomplete"
  | "manual"
  | "unknown";

export interface ChecklistStep {
  /** Stable id; used as the test selector + key. */
  readonly id: ChecklistStepId;
  /** Short imperative copy. */
  readonly label: string;
  readonly status: ChecklistStepStatus;
  /** One-line honest detail. Never implies a state we cannot prove. */
  readonly detail: string;
  /** Optional in-app navigation target for this step. */
  readonly cta?: { readonly label: string; readonly view: AppViewId };
}

export type ChecklistStepId =
  | "generate-identity"
  | "install-public-key"
  | "create-host"
  | "create-profile"
  | "host-key-trust"
  | "auth-check"
  | "launch-terminal";

/**
 * Map a {@link LoadResult} to a {@link CardState}. `null` represents
 * "not yet loaded" — distinct from a load failure (which surfaces as
 * `unavailable`).
 */
export function cardStateFromLoad<T>(
  result: LoadResult<T[]> | null,
): CardState {
  if (result === null) return { kind: "loading" };
  if (!result.ok) return { kind: "unavailable" };
  return { kind: "ready", value: result.data.length };
}

/**
 * Bundle the four inventory loads into a single {@link InventoryCounts}
 * snapshot. Each result is independent; one failure does not poison
 * the others.
 */
export function summarizeInventory(args: {
  hosts: LoadResult<Host[]> | null;
  profiles: LoadResult<ServerProfile[]> | null;
  identities: LoadResult<SshIdentity[]> | null;
  sessions: LoadResult<TerminalSession[]> | null;
}): InventoryCounts {
  return {
    hosts: cardStateFromLoad(args.hosts),
    profiles: cardStateFromLoad(args.profiles),
    identities: cardStateFromLoad(args.identities),
    sessions: cardStateFromLoad(args.sessions),
  };
}

/**
 * Breakdown of terminal-session counts by status. Returns `loading`
 * before the first load completes and `unavailable` on a load failure;
 * an empty list is `ready` with all zeros.
 */
export function summarizeSessionStatuses(
  result: LoadResult<TerminalSession[]> | null,
): SessionStatusBreakdown {
  if (result === null) return { kind: "loading" };
  if (!result.ok) return { kind: "unavailable" };
  const counts: SessionStatusCounts = {
    starting: 0,
    active: 0,
    detached: 0,
    closed: 0,
  };
  for (const s of result.data) {
    counts[s.status] += 1;
  }
  return { kind: "ready", counts, total: result.data.length };
}

/**
 * Pure mapping from a known {@link TerminalSessionStatus} to the
 * summary-card label. Kept here (rather than re-using
 * `terminal/sessionStatus.ts`'s {@link statusLabel}) so the dashboard
 * does not pull the per-row status helper into its dependency surface.
 */
const SESSION_STATUS_ORDER: readonly TerminalSessionStatus[] = [
  "active",
  "detached",
  "starting",
  "closed",
];

export function sessionStatusOrder(): readonly TerminalSessionStatus[] {
  return SESSION_STATUS_ORDER;
}

/**
 * Derive the connection-flow setup checklist from the inventory
 * snapshot. The function is total: it returns the same seven steps in
 * the same order regardless of input. Steps that cannot be safely
 * inferred from current API data stay `manual` — the dashboard never
 * promises a state it has not actually observed.
 */
export function deriveChecklist(inv: InventoryCounts): readonly ChecklistStep[] {
  const idStatus = stepStatusFromCount(inv.identities);
  const hostStatus = stepStatusFromCount(inv.hosts);
  const profileStatus = stepStatusFromCount(inv.profiles);
  const sessionStatus = stepStatusFromCount(inv.sessions);
  return [
    {
      id: "generate-identity",
      label: "Generate an SSH identity",
      status: idStatus,
      detail:
        "RelayTerm generates the keypair on the backend; only the public key leaves the vault.",
      cta: { label: "Open SSH identities", view: "identities" },
    },
    {
      id: "install-public-key",
      label: "Install the public key on the target server",
      status: "manual",
      detail:
        "Copy the public key to the server's authorized_keys yourself; the dashboard cannot tell whether the install succeeded.",
      cta: { label: "Open SSH identities", view: "identities" },
    },
    {
      id: "create-host",
      label: "Create a host",
      status: hostStatus,
      detail:
        "A host is a metadata-only target definition (display name, hostname, port, default username). No SSH connection is attempted.",
      cta: { label: "Open servers", view: "servers" },
    },
    {
      id: "create-profile",
      label: "Create a server profile",
      status: profileStatus,
      detail:
        "A profile links a host, an SSH identity, and an optional username override.",
      cta: { label: "Open servers", view: "servers" },
    },
    {
      id: "host-key-trust",
      label: "Run host-key preflight and trust the result",
      status: "manual",
      detail:
        "Verify and trust the captured host-key fingerprint from the server profile row in the Servers view.",
      cta: { label: "Open servers", view: "servers" },
    },
    {
      id: "auth-check",
      label: "Run the auth-check",
      status: "manual",
      detail:
        "Confirm public-key authentication succeeds from the server profile row in the Servers view.",
      cta: { label: "Open servers", view: "servers" },
    },
    {
      id: "launch-terminal",
      label: "Launch a terminal",
      status: sessionStatus,
      detail:
        "Launch from a server profile row; the dashboard reports a session has been started, not that the next launch will succeed.",
      cta: { label: "Open terminal", view: "terminal" },
    },
  ];
}

function stepStatusFromCount(card: CardState): ChecklistStepStatus {
  if (card.kind === "loading") return "unknown";
  if (card.kind === "unavailable") return "unknown";
  return card.value > 0 ? "complete" : "incomplete";
}

/**
 * Stable navigation actions exposed on the dashboard. Pinned in tests
 * against drift between the helper and the production route table.
 */
export interface NavigationAction {
  readonly id: string;
  readonly label: string;
  readonly view: AppViewId;
  readonly path: AppRoutePath;
}

export const DASHBOARD_NAV_ACTIONS: readonly NavigationAction[] = [
  {
    id: "manage-servers",
    label: "Manage servers",
    view: "servers",
    path: "/servers",
  },
  {
    id: "manage-identities",
    label: "Manage SSH identities",
    view: "identities",
    path: "/identities",
  },
  {
    id: "open-terminal",
    label: "Open terminal",
    view: "terminal",
    path: "/terminal",
  },
  {
    id: "view-sessions",
    label: "View sessions",
    view: "sessions",
    path: "/sessions",
  },
  {
    id: "configure-terminal",
    label: "Configure terminal",
    view: "settings",
    path: "/settings",
  },
] as const;
