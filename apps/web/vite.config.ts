import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      // The terminal attach endpoint is `/api/v1/terminal-sessions/:id/ws`,
      // a WebSocket upgrade under the `/api` prefix. String-form proxy
      // entries do not forward WebSocket upgrades, so the object form
      // with `ws: true` is required for the production terminal to
      // attach under `vite dev`. REST traffic on `/api` continues to
      // work the same way.
      "/api": {
        target: "http://127.0.0.1:8080",
        ws: true,
      },
      "/healthz": "http://127.0.0.1:8080",
      "/ws": {
        target: "ws://127.0.0.1:8080",
        ws: true,
      },
    },
  },
});
