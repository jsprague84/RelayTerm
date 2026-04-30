/**
 * Frontend helper for `GET /healthz`.
 *
 * Scope: production app shell uses this from the dashboard view to render
 * a backend health badge. It is a one-shot probe — no polling, no retry
 * — and does not authenticate. The backend's `/healthz` is a static 200
 * with no body; the helper only inspects `response.ok`.
 *
 * The helper does NOT throw; transport errors collapse to `"down"` so the
 * caller can render a single string without try/catch noise. The
 * underlying error is intentionally not surfaced — `/healthz` is a
 * liveness probe, not a diagnostic. Operator detail belongs in server
 * logs, never in the browser console.
 */

export type HealthStatus = "unknown" | "ok" | "down";

export interface CheckHealthOptions {
  /** Replaceable for tests. Defaults to `globalThis.fetch` resolved at
   * call time so a polyfill or test patch installed after this module
   * loads is still picked up. */
  fetchImpl?: typeof fetch;
  /** Replaceable for tests. Defaults to `/healthz`. */
  endpoint?: string;
}

export async function checkHealth(
  options: CheckHealthOptions = {},
): Promise<HealthStatus> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") return "down";
  const endpoint = options.endpoint ?? "/healthz";
  try {
    const res = await fetchImpl(endpoint);
    return res.ok ? "ok" : "down";
  } catch {
    return "down";
  }
}
