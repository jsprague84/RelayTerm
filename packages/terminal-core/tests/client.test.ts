import { describe, expect, it } from "vitest";
import {
  TerminalSessionClient,
  type ClientMsg,
  type SessionAttachedMsg,
  type SessionClosedMsg,
  type SessionDetachedMsg,
  type TerminalClientError,
  type TerminalSessionState,
} from "../src/index.js";
import { FakeTransport } from "./fakeTransport.js";

const ATTACH_OK: SessionAttachedMsg = {
  type: "session_attached",
  session_id: "ses-1",
  attachment_id: "att-1",
  status: "attached_stub",
  message: "attached to RelayTerm session placeholder",
};

function makeClient() {
  const transport = new FakeTransport();
  const client = new TerminalSessionClient({ transport });
  const states: TerminalSessionState[] = [];
  client.on("state_change", (s) => states.push(s));
  return { transport, client, states };
}

describe("TerminalSessionClient", () => {
  it("transitions idle → connecting → attached on the canonical happy path", async () => {
    const { transport, client, states } = makeClient();
    expect(client.state).toBe("idle");
    const attaching = client.attach({ url: "ws://test/ws", sessionId: "ses-1" });
    await attaching;
    expect(client.state).toBe("connecting");

    transport.simulateServerMsg(ATTACH_OK);
    expect(client.state).toBe("attached");
    expect(states).toEqual(["connecting", "attached"]);

    expect(transport.sent[0]).toEqual<ClientMsg>({
      type: "attach",
      session_id: "ses-1",
      last_seen_seq: null,
      client_id: null,
    });
  });

  it("attached → detached on session_detached frame", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    let lastDetach: SessionDetachedMsg | null = null;
    client.on("detached", (e) => (lastDetach = e));
    const detached: SessionDetachedMsg = {
      type: "session_detached",
      session_id: "ses-1",
      attachment_id: "att-1",
    };
    transport.simulateServerMsg(detached);
    expect(client.state).toBe("detached");
    expect(lastDetach).toEqual(detached);
  });

  it("attached → closed on session_closed frame", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    let lastClose: SessionClosedMsg | null = null;
    client.on("closed", (e) => (lastClose = e));
    const closed: SessionClosedMsg = {
      type: "session_closed",
      session_id: "ses-1",
    };
    transport.simulateServerMsg(closed);
    expect(client.state).toBe("closed");
    expect(lastClose).toEqual(closed);
  });

  it("emits error and goes to error state on transport-error event", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    const errors: TerminalClientError[] = [];
    client.on("error", (e) => errors.push(e));
    transport.simulateTransportError({ kind: "network" });
    expect(errors[0]).toMatchObject({ kind: "transport" });
  });

  it("collapses to error when the first frame is not session_attached", async () => {
    const { transport, client } = makeClient();
    const errors: TerminalClientError[] = [];
    client.on("error", (e) => errors.push(e));
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg({ type: "pong" });
    expect(client.state).toBe("error");
    expect(errors[0]?.kind).toBe("unexpected_first_frame");
  });

  it("rejects input before attached without including the payload", async () => {
    const { client } = makeClient();
    const sentinel = "REDACT-MARKER-INPUT-7C42";
    const errors: TerminalClientError[] = [];
    const rejections: { reason: string; attempted: string }[] = [];
    client.on("error", (e) => errors.push(e));
    client.on("input_rejected_or_stubbed", (r) =>
      rejections.push({ reason: r.reason, attempted: r.attempted }),
    );
    client.sendInput(sentinel);
    expect(rejections[0]).toEqual({
      reason: "not_attached",
      attempted: "input",
    });
    for (const err of errors) {
      expect(JSON.stringify(err)).not.toContain(sentinel);
    }
    for (const rej of rejections) {
      expect(JSON.stringify(rej)).not.toContain(sentinel);
    }
  });

  it("ignores resize before attached", async () => {
    const { transport, client } = makeClient();
    const errors: TerminalClientError[] = [];
    client.on("error", (e) => errors.push(e));
    client.sendResize(80, 24);
    expect(transport.sent).toHaveLength(0);
    expect(errors[0]?.kind).toBe("send_before_attached");
  });

  it("translates pty_not_implemented errors into stubbed-rejection events", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    const rejections: { reason: string; attempted: string }[] = [];
    const errors: TerminalClientError[] = [];
    client.on("input_rejected_or_stubbed", (r) =>
      rejections.push({ reason: r.reason, attempted: r.attempted }),
    );
    client.on("error", (e) => errors.push(e));
    transport.simulateServerMsg({
      type: "error",
      code: "pty_not_implemented",
      message: "PTY streaming is not implemented yet",
    });
    expect(rejections).toEqual([
      { reason: "pty_not_implemented", attempted: "input" },
    ]);
    // Critical: the stubbed-rejection path must NOT also fire the generic
    // `error` event, or consumers listening to both will react twice.
    expect(errors).toEqual([]);
  });

  it("still emits error for non-stub server errors", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    const errors: TerminalClientError[] = [];
    client.on("error", (e) => errors.push(e));
    transport.simulateServerMsg({
      type: "error",
      code: "internal",
      message: "internal error",
    });
    expect(errors[0]).toMatchObject({
      kind: "server_error",
      code: "internal",
    });
  });

  it("emits a synthetic detached event when transport drops while attached", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    let detachPayload: { session_id: string; attachment_id: string } | null =
      null;
    client.on("detached", (m) => {
      detachPayload = { session_id: m.session_id, attachment_id: m.attachment_id };
    });
    transport.simulateClose({ code: 1006 });
    expect(client.state).toBe("detached");
    expect(detachPayload).toEqual({
      session_id: ATTACH_OK.session_id,
      attachment_id: ATTACH_OK.attachment_id,
    });
  });

  it("treats detach() and close() before attach as silent no-ops", async () => {
    const { transport, client } = makeClient();
    const errors: TerminalClientError[] = [];
    const rejections: unknown[] = [];
    client.on("error", (e) => errors.push(e));
    client.on("input_rejected_or_stubbed", (r) => rejections.push(r));
    client.detach();
    client.close();
    expect(transport.sent).toEqual([]);
    expect(errors).toEqual([]);
    expect(rejections).toEqual([]);
  });

  it("emits resize_ack on ack { kind: resize }", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);

    let acks = 0;
    client.on("resize_ack", () => acks++);
    transport.simulateServerMsg({ type: "ack", kind: "resize" });
    expect(acks).toBe(1);
  });

  it("transitions to error if the transport closes before attach completes", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateClose({ code: 1006 });
    expect(client.state).toBe("error");
  });

  it("disposes cleanly without leaving listeners attached", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);
    client.dispose();
    let pongs = 0;
    client.on("pong", () => pongs++);
    transport.simulateServerMsg({ type: "pong" });
    expect(pongs).toBe(0);
  });

  it("forwards lastSeenSeq into the attach frame", async () => {
    const { transport, client } = makeClient();
    await client.attach({
      url: "ws://test/ws",
      sessionId: "ses-1",
      lastSeenSeq: 42,
    });
    expect(transport.sent[0]).toEqual<ClientMsg>({
      type: "attach",
      session_id: "ses-1",
      last_seen_seq: 42,
      client_id: null,
    });
  });

  it("tracks lastSeenSeq across live output frames", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws" });
    transport.simulateServerMsg(ATTACH_OK);
    expect(client.lastSeenSeq).toBe(0);

    transport.simulateServerMsg({ type: "output", seq: 7, data: "" });
    expect(client.lastSeenSeq).toBe(7);
    transport.simulateServerMsg({ type: "output", seq: 12, data: "" });
    expect(client.lastSeenSeq).toBe(12);
    // Out-of-order arrival never lowers the bookmark — the renderer
    // wants the highest seq it has actually seen.
    transport.simulateServerMsg({ type: "output", seq: 9, data: "" });
    expect(client.lastSeenSeq).toBe(12);
  });

  it("emits replay_start, replay_end, and advances lastSeenSeq", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws", lastSeenSeq: 4 });
    transport.simulateServerMsg(ATTACH_OK);

    const events: Array<{
      type: string;
      from_seq?: number;
      to_seq?: number;
      latest_seq?: number;
    }> = [];
    client.on("replay_start", (m) =>
      events.push({ type: m.type, from_seq: m.from_seq, to_seq: m.to_seq }),
    );
    client.on("replay_end", (m) =>
      events.push({ type: m.type, latest_seq: m.latest_seq }),
    );

    transport.simulateServerMsg({
      type: "replay_start",
      from_seq: 5,
      to_seq: 7,
    });
    transport.simulateServerMsg({ type: "output", seq: 5, data: "" });
    transport.simulateServerMsg({ type: "output", seq: 6, data: "" });
    transport.simulateServerMsg({ type: "output", seq: 7, data: "" });
    transport.simulateServerMsg({ type: "replay_end", latest_seq: 7 });

    expect(events).toEqual([
      { type: "replay_start", from_seq: 5, to_seq: 7 },
      { type: "replay_end", latest_seq: 7 },
    ]);
    expect(client.lastSeenSeq).toBe(7);
  });

  it("emits replay_window_lost with safe metadata only", async () => {
    const { transport, client } = makeClient();
    await client.attach({ url: "ws://test/ws", lastSeenSeq: 1 });
    transport.simulateServerMsg(ATTACH_OK);

    const sentinel = "REDACT-MARKER-WINDOW-LOST-3F";
    let payload: unknown = null;
    client.on("replay_window_lost", (m) => {
      payload = m;
    });

    transport.simulateServerMsg({
      type: "replay_window_lost",
      requested_seq: 1,
      oldest_available_seq: 12,
      latest_seq: 20,
    });
    expect(JSON.stringify(payload)).not.toContain(sentinel);
    expect(payload).toEqual({
      type: "replay_window_lost",
      requested_seq: 1,
      oldest_available_seq: 12,
      latest_seq: 20,
    });
    // The renderer is expected to skip ahead — the bookmark advances
    // to latest_seq so the next attach starts from the post-loss point.
    expect(client.lastSeenSeq).toBe(20);
  });
});
