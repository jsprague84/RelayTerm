/**
 * Mobile portrait nav drawer (fix/mobile-portrait-sidebar-ux).
 *
 * Below the `sm:` breakpoint (640px) the persistent left sidebar would
 * consume ~14rem of a ~20rem-wide Android portrait viewport, leaving
 * almost no room for content. The fix turns `SidebarNav` into a
 * slide-in drawer at narrow widths and surfaces a hamburger toggle in
 * `TopBar`. From `sm:` and up the layout is byte-identical to before
 * (persistent column).
 *
 * This file is a static text-scan harness (same style as
 * `appShellIsolation.test.ts` and `mobileIdentifierInputs.test.ts`) —
 * no jsdom, no Svelte mount. It pins:
 *   - the four mobile-nav `data-testid` selectors exist
 *   - the responsive escape hatch (`sm:` classes) is present so the
 *     desktop layout cannot regress to "drawer-only" by accident
 *   - the toggle's `aria-controls` matches the drawer `id`
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));

const SIDEBAR_PATH = resolve(
  __dirname,
  "../src/lib/app/SidebarNav.svelte",
);
const TOP_BAR_PATH = resolve(__dirname, "../src/lib/app/TopBar.svelte");
const APP_SHELL_PATH = resolve(__dirname, "../src/lib/app/AppShell.svelte");

const sidebar = () => readFileSync(SIDEBAR_PATH, "utf8");
const topBar = () => readFileSync(TOP_BAR_PATH, "utf8");
const appShell = () => readFileSync(APP_SHELL_PATH, "utf8");

describe("mobile nav drawer — selectors", () => {
  it("TopBar exposes a hamburger toggle hidden from sm: and up", () => {
    const text = topBar();
    expect(text).toMatch(/data-testid="app-mobile-nav-toggle"/);
    // The button must be sm:hidden so it disappears on tablet/desktop
    // where the persistent sidebar is back. Token may appear before
    // or after the `data-testid=` attribute on the same element.
    expect(text).toMatch(
      /sm:hidden[\s\S]{0,400}data-testid="app-mobile-nav-toggle"|data-testid="app-mobile-nav-toggle"[\s\S]{0,400}sm:hidden/,
    );
  });

  it("SidebarNav exposes drawer, close, and backdrop selectors", () => {
    const text = sidebar();
    expect(text).toMatch(/data-testid="app-mobile-nav-drawer"/);
    expect(text).toMatch(/data-testid="app-mobile-nav-close"/);
    expect(text).toMatch(/data-testid="app-mobile-nav-backdrop"/);
  });

  it("close button and backdrop are mobile-only (sm:hidden)", () => {
    const text = sidebar();
    // Close button: hidden on sm: and up so it doesn't appear next to
    // the brand on desktop. The `sm:hidden` token may appear in either
    // the class attribute (before `data-testid=`) or another attribute
    // (after); match both directions.
    expect(text).toMatch(
      /sm:hidden[\s\S]{0,400}data-testid="app-mobile-nav-close"|data-testid="app-mobile-nav-close"[\s\S]{0,400}sm:hidden/,
    );
    // Backdrop: only renders on mobile.
    expect(text).toMatch(
      /sm:hidden[\s\S]{0,400}data-testid="app-mobile-nav-backdrop"|data-testid="app-mobile-nav-backdrop"[\s\S]{0,400}sm:hidden/,
    );
  });
});

describe("mobile nav drawer — desktop layout preserved", () => {
  it("SidebarNav restores static positioning at sm: and up", () => {
    const text = sidebar();
    // Below sm: the aside is fixed + translated. At sm: and up we
    // must restore static positioning AND zero out the translate so
    // the persistent column behaviour is byte-identical to before.
    expect(text).toMatch(/sm:static/);
    expect(text).toMatch(/sm:translate-x-0/);
  });

  it("toggle's aria-controls points at the drawer id", () => {
    // Without the id wiring, screen readers cannot follow the
    // "expand → here is what expanded" relationship.
    expect(topBar()).toMatch(/aria-controls="app-mobile-nav-drawer"/);
    expect(sidebar()).toMatch(/id="app-mobile-nav-drawer"/);
  });
});

describe("mobile nav drawer — AppShell wiring", () => {
  it("AppShell owns ephemeral drawer state and passes it to both children", () => {
    const text = appShell();
    // Ephemeral $state — never persisted to localStorage / cookie /
    // URL. Pinned here so a future change that tries to durably
    // remember the drawer state has to update this test first.
    expect(text).toMatch(/let mobileNavOpen = \$state\(false\)/);
    // The toggle handler flips the state.
    expect(text).toMatch(/onToggleMobileNav=\{\(\) => \(mobileNavOpen = !mobileNavOpen\)\}/);
    // SidebarNav receives the open flag and a close callback.
    expect(text).toMatch(/isOpen=\{mobileNavOpen\}/);
    expect(text).toMatch(/onClose=\{\(\) => \(mobileNavOpen = false\)\}/);
  });
});
