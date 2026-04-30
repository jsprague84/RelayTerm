import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Tests import `describe`/`it`/`expect` explicitly from `vitest`, so the
    // `globals: true` shortcut is unnecessary here. Keeping globals off
    // means tsconfig doesn't need a `"types": ["vitest/globals"]` entry.
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
});
