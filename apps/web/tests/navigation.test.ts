import { describe, it, expect } from "vitest";
import {
  DEFAULT_VIEW,
  NAV_ITEMS,
  findNavItem,
  type AppViewId,
} from "../src/lib/app/navigation.js";

describe("navigation", () => {
  it("starts at the dashboard view", () => {
    expect(DEFAULT_VIEW).toBe("dashboard");
    expect(NAV_ITEMS[0]?.id).toBe(DEFAULT_VIEW);
  });

  it("exposes the production-facing sections in order", () => {
    expect(NAV_ITEMS.map((n) => n.id)).toEqual([
      "dashboard",
      "terminal",
      "sessions",
      "servers",
      "identities",
      "settings",
    ]);
  });

  it("uses unique ids", () => {
    const ids = NAV_ITEMS.map((n) => n.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("provides a non-empty label and description for every item", () => {
    for (const item of NAV_ITEMS) {
      expect(item.label.trim().length).toBeGreaterThan(0);
      expect(item.description.trim().length).toBeGreaterThan(0);
    }
  });

  it("does not surface dev-lab terminology in production labels", () => {
    // The dev lab uses words like "lab", "workbench", "diagnostic". The
    // production sidebar is user-facing copy and must not borrow them.
    const banned = ["lab", "workbench", "diagnostic"];
    for (const item of NAV_ITEMS) {
      const text = `${item.label} ${item.description}`.toLowerCase();
      for (const word of banned) {
        expect(text.includes(word)).toBe(false);
      }
    }
  });

  it("findNavItem returns the matching entry", () => {
    for (const item of NAV_ITEMS) {
      expect(findNavItem(item.id)).toBe(item);
    }
  });

  it("findNavItem throws on an unknown id", () => {
    expect(() => findNavItem("nope" as AppViewId)).toThrowError(
      /unknown view id/,
    );
  });
});
