import { describe, it, expect, vi } from "vitest";
import { checkHealth } from "../src/lib/api/health.js";

function jsonResponse(status: number): Response {
  return new Response(null, { status });
}

describe("checkHealth", () => {
  it("returns 'ok' on a 2xx response", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse(200));
    const result = await checkHealth({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result).toBe("ok");
    expect(fetchImpl).toHaveBeenCalledWith("/healthz");
  });

  it("returns 'down' on a non-2xx response", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse(503));
    const result = await checkHealth({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result).toBe("down");
  });

  it("returns 'down' when the transport rejects", async () => {
    const fetchImpl = vi.fn().mockRejectedValue(new Error("network down"));
    const result = await checkHealth({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result).toBe("down");
  });

  it("uses the provided endpoint override", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse(200));
    await checkHealth({
      fetchImpl: fetchImpl as unknown as typeof fetch,
      endpoint: "/_internal/health",
    });
    expect(fetchImpl).toHaveBeenCalledWith("/_internal/health");
  });

  it("does not log or surface transport error detail", async () => {
    // The probe is a liveness signal, not a diagnostic. The thrown
    // message must not appear on the console — we assert no console
    // calls happened during the failed probe.
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fetchImpl = vi
      .fn()
      .mockRejectedValue(new Error("super-secret-internal-detail"));
    const result = await checkHealth({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result).toBe("down");
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });
});
