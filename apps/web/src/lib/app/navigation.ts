/**
 * Production app shell navigation model.
 *
 * The shell uses a small local view-state model, not a routing library:
 * each entry's `id` is the discriminator the shell switches on. Adding a
 * router is its own slice — see SPEC.md "Production web app shell". Order
 * here is the rendered order in the sidebar; first entry is the default
 * landing view.
 *
 * Placeholder views are intentionally non-functional. They exist so the
 * shell layout, navigation, and dev/prod gating can ship before the real
 * CRUD/terminal/auth UI lands. See `views/` for per-view copy.
 */

export type AppViewId =
  | "dashboard"
  | "terminal"
  | "sessions"
  | "servers"
  | "identities"
  | "settings";

export interface NavItem {
  readonly id: AppViewId;
  readonly label: string;
  readonly description: string;
}

export const NAV_ITEMS: readonly NavItem[] = [
  {
    id: "dashboard",
    label: "Dashboard",
    description: "Overview and backend health",
  },
  {
    id: "terminal",
    label: "Terminal",
    description: "Live SSH terminal workspace",
  },
  {
    id: "sessions",
    label: "Sessions",
    description: "Reconnectable terminal sessions",
  },
  {
    id: "servers",
    label: "Server profiles",
    description: "Saved SSH connection profiles",
  },
  {
    id: "identities",
    label: "SSH identities",
    description: "Public keys for authorized_keys",
  },
  {
    id: "settings",
    label: "Settings",
    description: "Renderer, theme, and preferences",
  },
] as const;

export const DEFAULT_VIEW: AppViewId = NAV_ITEMS[0].id;

export function findNavItem(id: AppViewId): NavItem {
  const item = NAV_ITEMS.find((n) => n.id === id);
  if (!item) {
    throw new Error(`unknown view id: ${id as string}`);
  }
  return item;
}
