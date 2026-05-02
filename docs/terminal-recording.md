# Durable terminal recording and replay architecture

> Design slice. No code, no migrations, no UI in this slice.
> Anchored from `SPEC.md` → "Durable terminal recording and replay architecture".
> AGENTS.md governs *how* code is written; this doc governs *what* the
> recording subsystem will eventually do, and — load-bearing — what it
> must never do.

This document is normative for any future recording / replay / restart-recovery
work. Drift from any rule below is a spec bug, not an implementation freedom.
The goal is to make the load-bearing invariants explicit *before* a single
byte is persisted, so the implementation slices can be small, separable, and
fail-closed by default.

---

## 1. Current state (what already exists)

Recording layers on top of these existing contracts. Read them first:

- **Live SSH PTY bridge** (`SPEC.md` → "Live SSH PTY bridge contract").
  `POST /api/v1/terminal-sessions` allocates a real russh PTY, the WebSocket
  attach forwards `output` server→client and `input` / `resize` client→server.
- **Binary terminal data envelope** (`SPEC.md` → "Terminal data plane:
  binary envelope"). Hot-path output/input rides binary `RTB1` frames; the
  control plane stays JSON. `Output.payload_len <= 1 MiB`.
- **Output sequence + in-memory replay buffer** (`SPEC.md` → "Output
  sequence + in-memory replay buffer contract"). Each PTY output frame
  carries a monotonic per-session `seq` starting at `1` and incrementing by
  exactly one. The orchestrator's PTY forwarder is the **single** seq
  assigner.
- **Replay buffer policy**: `ReplayBufferConfig` defaults to
  `max_frames = 1024` AND `max_bytes = 1 MiB` (whichever bound is hit first).
  FIFO eviction. The most recent frame is always retained even when it
  overshoots `max_bytes`.
- **Replay handshake**: `Attach { last_seen_seq: Some(n) }` triggers
  `ReplayStart { from_seq, to_seq }` → buffered `Output` frames →
  `ReplayEnd { latest_seq }`. A bookmark older than the buffer's oldest
  retained frame surfaces a single `ReplayWindowLost { requested_seq,
  oldest_available_seq, latest_seq }` and live attach continues.
- **Detached-session TTL**: `DETACHED_LIVE_PTY_TTL = 30s` (in
  `crates/relayterm-terminal/src/manager.rs`). The PTY, the broadcast
  channel, and the replay buffer all live in the orchestrator's runtime
  registry and survive the *last* detach for this bounded window. A
  reattach inside the window cancels the close and transitions back to
  `active`. Lose the PTY → lose the buffer.
- **Active-session local recovery pointer** (`SPEC.md` → "Production
  active terminal local recovery"). The browser stashes the
  `(session_id, last_seen_seq)` pointer in `sessionStorage` so a tab
  reload can re-attach inside the TTL window. This is a **client-side
  hint**, not a recording — it carries no terminal bytes.
- **Closed sessions historical metadata**: rows in `terminal_sessions`
  survive a close. `session_events` retains the lifecycle log
  (`created`, `attached`, `resized`, `detached`, `reattached`, `closed`).
  Neither table holds terminal bytes today.
- **No backend restart recovery today**. A restart drops the in-memory
  runtime registry — every live PTY, every broadcast channel, every
  replay buffer. Any pre-restart `starting` / `active` / `detached` row
  is operator-visible as a stale metadata record until it is explicitly
  closed via `POST /:id/close`.
- **Audit-events forbidden-substring rule** (`AGENTS.md` → "Things to
  avoid", `crates/relayterm-api/tests/api.rs` → `AUDIT_FORBIDDEN_SUBSTRINGS`).
  `audit_events.payload` is **public metadata only**. Terminal I/O,
  replay frames, peer banners, `client_info`, encrypted-key bytes, PEM
  markers — none of it is permitted in any audit row.

The architectural rule from `AGENTS.md` is the spine: **the SSH session
belongs to the backend; the terminal renderer belongs to the frontend;
the terminal state belongs to the session orchestrator; the client may
disappear and come back.** Recording is an extension of "terminal state
belongs to the orchestrator" — the orchestrator is the only writer.

---

## 2. Goals

The recording subsystem exists to make the following true. Every later
slice should trace its work back to one of these.

1. **Reconnect after browser reload / network drop / detach within the
   in-memory replay window** — already true today via the in-memory ring.
   Recording must not regress this path.
2. **Reconnect after detach beyond the in-memory replay window** — when
   the bookmark predates the buffer's oldest retained frame. Today this
   surfaces `ReplayWindowLost` and the renderer resets its grid; with
   durable chunks, the orchestrator can replay older frames from disk,
   then resume live fanout from the in-memory ring. The wire path remains
   `ReplayStart` → `Output` → `ReplayEnd`; the *source* of the replayed
   frames is an implementation detail.
3. **Display-history recovery after a backend restart** — the live SSH
   PTY cannot be resurrected (russh state is gone, the remote shell
   continues running unattached on the target). Durable chunks let an
   operator open a closed-with-recoverable-display view of what the
   pre-restart session printed up to the last persisted seq.
4. **Operator review of a closed session's transcript** — a session that
   ended (clean close, PTY teardown, ssh_start_failed, TTL expiry) leaves
   a durable transcript an owner can scroll through later.
5. **A future audit / compliance boundary that does NOT put terminal
   I/O into `audit_events`**. Recording lives in its own table family,
   owner-scoped, with its own retention and access path. Audit
   continues to carry public metadata only.
6. **Renderer-neutral byte stream**. Recording stores the same bytes
   the live wire carries (the `Output.payload` of the binary `RTB1`
   envelope). Any renderer (xterm, ghostty-web, restty, wterm, future
   native) can consume the same chunks via the same `write(Uint8Array)`
   path it already uses for live frames. No grid-state, no shaper state,
   no per-renderer glyph state is persisted.
7. **No new logging surface for secrets**. Recording captures what the
   operator's terminal already showed. It does not make terminal output
   *more* visible than the live wire already made it; it makes it
   visible *for longer*. The privacy posture (Section 7) is the bar.

## 3. Non-goals

These are explicitly out of scope. A future slice that wants any of
these must update this document first, with rationale.

- **Keystroke auditing in `audit_events`**. Input is never recorded by
  default (Section 4) and never appears in any audit row.
- **Command parsing**. The recording layer treats the byte stream as
  opaque PTY output. It does not split into "commands", does not know
  about prompts, does not detect shells.
- **Shell-aware redaction**. The orchestrator does not attempt to spot
  passwords, SSH-agent prompts, sudo prompts, base64 blobs, etc. The
  paste-safety policy is shape-based at the *client* (newlines, size,
  control chars); recording is not the place to re-litigate that.
- **Guaranteed secret-free transcripts**. Recordings may contain secrets
  the operator's session printed to the terminal. The privacy posture
  (Section 7) treats the corpus as sensitive and bounds blast radius;
  it does not pretend to sanitise content.
- **Collaborative multi-writer semantics**. One PTY, one writer
  (the orchestrator). Multi-client *read* attach is future work and is
  orthogonal to recording.
- **Legal / compliance certification**. RelayTerm is single-tenant
  self-hosted. The operator is responsible for their own retention,
  encryption-at-rest, and disclosure decisions. This doc names the
  knobs; it does not promise SOC-2 / HIPAA / PCI scope.
- **Mobile-specific replay UX**. The Tauri / Android shell currently
  wraps `apps/web`. Recording UX in the production web app comes
  before any mobile-specific replay surface.

## 4. What to persist

The recording subsystem has four classes of data it *could* persist.
Only some are in scope.

### 4.1 Output frame bytes — IN SCOPE (v1 target)

Persist the same bytes that flow through `Output { seq, data }` on the
live wire. Group adjacent frames into chunks (Section 5). Each chunk
records `seq_start`, `seq_end`, the concatenated payload bytes, and a
short metadata header.

**Why**: this is what the user already saw. The renderer's existing
`write(Uint8Array)` path reproduces the original visual state byte-for-byte
(modulo VT state interactions across chunk boundaries, which the renderer
already handles for live frames). This keeps the contract
renderer-neutral — there is no grid model, no shaper model, no font
model in the persisted form.

### 4.2 Resize / lifecycle markers — IN SCOPE (v1 target)

Persist a small, separate stream of markers tagged with the seq at
which they took effect: `started`, `attached`, `detached`,
`reattached`, `resized { cols, rows }`, `closed { reason, category? }`,
and (only if needed) `replay_gap { from_seq, to_seq, reason }` for
spans the recording layer knows it lost.

**Why**: a player needs to know "the terminal was 80×24 here, then
resized to 132×40 at seq=4017" to render correctly. Markers are
*metadata*, not bytes; they sit in their own table so a fast metadata
scan does not have to sweep the chunk table.

### 4.3 Input frame bytes — NOT IN SCOPE FOR V1

Do not persist client → server input.

**Why**: input includes passwords typed at a non-echoing prompt,
sudo passwords, vault decryption phrases, paste content the user
explicitly confirmed. The wire path already redacts input from every
log surface (`AGENTS.md` → "Logging and reflection prohibitions").
Input recording would be a *new* surface that holds the riskiest
bytes for *longer* than the live wire ever did. The risk/reward for
v1 does not justify it.

If an operator-driven keystroke-audit slice ever lands, it must be:
- explicitly opt-in per session, via the `server_profile` (not a
  global default);
- separated from the output recording table family entirely;
- gated by an explicit "I have read what this captures" warning in
  the create flow;
- redaction-aware (control-char filtering at minimum).

This doc does not design that surface. Section 13 lists it as an
optional late-stage slice; it remains out of scope until the
operator-facing UI for recording itself has shipped and been used.

### 4.4 VT snapshots — NOT IN SCOPE FOR V1; future optimisation

A `libghostty-vt` (or equivalent) observer running alongside the PTY
forwarder can checkpoint a structured grid state at intervals. A
client opening a long replay can fast-forward to the nearest snapshot
and replay only the chunks past it.

**Why deferred**: the byte stream is the load-bearing replay surface
because every renderer already speaks it. A snapshot is an
*optimisation* on top of the byte stream, not a replacement. Shipping
snapshots first would either lock in a renderer-specific grid model
or duplicate the renderer's parse path on the server. Section 13
schedules this as the post-v1 perf slice.

### 4.5 What is intentionally NOT recorded

To make the boundary explicit:

- **Input bytes** (4.3). No keystroke capture in v1.
- **Decrypted private-key bytes**. Already wiped on drop in
  `SshAuthCheckService` and the PTY bridge; recording does not
  re-introduce them.
- **Peer banner / russh internal error text**. These are already
  redacted from every wire surface (`AGENTS.md` → "Logging and
  reflection prohibitions"); they are not part of the PTY output
  stream and never enter the chunk store.
- **`client_info` / `remote_addr`**. These are attachment metadata
  and live in `terminal_session_attachments` already. They are not
  copied into the recording tables.
- **Vault internals** (master key, envelope ciphertext from
  `ssh_identities`, magic prefix bytes). Recording uses its own
  envelope keying (Section 6.3).
- **Audit data**. Recording rows are not `audit_events` rows.
  `audit_events.payload` continues to obey the
  `AUDIT_FORBIDDEN_SUBSTRINGS` rule and never carries terminal
  output, paste content, or recording chunks.

## 5. Schema sketch

The migrations
`apps/backend/migrations/20260502000018_terminal_recording_chunks.sql`
and
`apps/backend/migrations/20260502000019_terminal_recording_markers.sql`
are the binding contract; the columns / CHECKs / indexes they create
are documented below.

**Note on `encryption` / `compression` typing.** The original sketch
used `SMALLINT` enums; the landed migrations use `TEXT` enums
(`'none'`, future `'recording_v1'` / `'zstd'`). TEXT keeps the column
self-describing in `psql` and matches the
`session_events_kind_chk` / `audit_events_kind_chk` /
`terminal_recording_markers_kind_chk` pattern already in use across
the rest of the schema. Future encryption / compression schemes are
added by extending the existing CHECK in a follow-up migration; the
default for new rows stays `'none'`.

### 5.1 `terminal_recording_chunks` (landed)

| column                | type          | notes                                                                 |
|-----------------------|---------------|-----------------------------------------------------------------------|
| `id`                  | `UUID PK`     | server-assigned                                                       |
| `terminal_session_id` | `UUID NN FK` | `REFERENCES terminal_sessions(id) ON DELETE RESTRICT` (see 5.4)        |
| `seq_start`           | `BIGINT NN`  | inclusive lowest seq covered by this chunk; `>= 1`                    |
| `seq_end`             | `BIGINT NN`  | inclusive highest seq covered by this chunk; `>= seq_start`           |
| `byte_len`            | `INTEGER NN` | length of `payload` AFTER any compression / encryption; `>0 AND <= 2 MiB` |
| `payload`             | `BYTEA NN`   | the raw stored bytes (see Section 6); `octet_length(payload) = byte_len` |
| `compression`         | `TEXT NN`    | `'none'` (v1). Future `'zstd'` extends the CHECK (Section 6.2)        |
| `encryption`          | `TEXT NN`    | `'none'` (v1, opt-in). Future `'recording_v1'` extends the CHECK (Section 6.3) |
| `created_at`          | `TIMESTAMPTZ NN DEFAULT NOW()` |                                                              |

Constraints actually enforced (see migration):
- `terminal_recording_chunks_seq_start_chk`
- `terminal_recording_chunks_seq_end_chk`
- `terminal_recording_chunks_byte_len_chk`
- `terminal_recording_chunks_payload_len_chk`
- `terminal_recording_chunks_encryption_chk`
- `terminal_recording_chunks_compression_chk`
- `terminal_recording_chunks_session_seq_start_uq` (unique)
- index `terminal_recording_chunks_session_seq_idx` on
  `(terminal_session_id, seq_start)` for `from_seq` reads.

Notes on the cap rationale: `byte_len <= 2 MiB` is defence-in-depth
against a runaway chunk row. The chunk writer (a future slice) is the
primary bound; the CHECK is the hard upper bound. 2 MiB covers the
worst-case single 1 MiB binary-envelope `Output` frame plus envelope
overhead from a future encrypted-row scheme (Section 6.3 — XChaCha20
nonce + Poly1305 tag + magic + version ≈ 41 bytes), with comfortable
TOAST headroom. Lowering this CHECK below 1 MiB + envelope overhead
would create a write surface that silently fails for legitimate
workloads.

### 5.2 `terminal_recording_markers` (landed)

| column                | type          | notes                                                  |
|-----------------------|---------------|--------------------------------------------------------|
| `id`                  | `UUID PK`     | server-assigned                                        |
| `terminal_session_id` | `UUID NN FK` | `REFERENCES terminal_sessions(id) ON DELETE RESTRICT` |
| `kind`                | `TEXT NN`    | enum: `started` \| `attached` \| `detached` \| `reattached` \| `resized` \| `closed` \| `replay_gap` |
| `seq`                 | `BIGINT NN`  | the seq AT WHICH this marker is observed (see note)    |
| `payload`             | `JSONB NN DEFAULT '{}'` | public-safe metadata only (see 5.5)        |
| `created_at`          | `TIMESTAMPTZ NN DEFAULT NOW()` |                                       |

Constraints / indexes:
- `CHECK (kind IN (...))` for the enum (mirror the Rust enum and
  evolve via dedicated migrations, same shape as
  `session_events_kind_chk` and `audit_events_kind_chk`).
- `CHECK (seq >= 0)`. Note: the live-wire `Output` seq contract starts
  at `1` (`SPEC.md` → "Sequence number contract"); marker seq tolerates
  `0` ONLY as the deliberate sentinel for pre-first-output markers
  (the `started` marker is written when the PTY runtime is bound,
  which happens BEFORE the forwarder has stamped any `Output` frame —
  `seq = 0` reads as "at session open, before any output"). Every
  other marker kind MUST carry the seq of the actual output frame
  it brackets and therefore satisfies `seq >= 1`. The migration slice
  pins this with a paired test: the only kind allowed at `seq = 0`
  is `started`.
- Index `terminal_recording_markers_session_seq_idx` on
  `(terminal_session_id, seq, created_at)` — the `created_at` tail is
  the tiebreaker for the `ORDER BY seq ASC, created_at ASC` shape used
  by `list_markers`.

### 5.3 `terminal_vt_snapshots` (deferred to a later slice)

| column                | type          | notes                                                  |
|-----------------------|---------------|--------------------------------------------------------|
| `id`                  | `UUID PK`     |                                                        |
| `terminal_session_id` | `UUID NN FK` | `ON DELETE RESTRICT`                                   |
| `seq`                 | `BIGINT NN`  | snapshot taken AT this seq (next chunk to apply has `seq_start = seq + 1`) |
| `cols` / `rows`       | `INTEGER NN` | grid dims at snapshot time                             |
| `snapshot_blob`       | `BYTEA NN`   | opaque to the API; format is owned by the VT crate     |
| `format_version`      | `SMALLINT NN`| reserved for future format migrations                  |
| `byte_len`            | `INTEGER NN` |                                                        |
| `encryption`          | `SMALLINT NN`| matches the chunk envelope                             |
| `created_at`          | `TIMESTAMPTZ NN DEFAULT NOW()` |                                       |

A snapshot is *additional* to the chunk stream, not a replacement. The
chunk stream remains the load-bearing reconstruction path (Section 8).

### 5.4 Rules the schema must enforce

- **Foreign keys**: `terminal_session_id` references
  `terminal_sessions(id)` with `ON DELETE RESTRICT`. Recordings
  outlive their session row only via deliberate retention sweeps
  (Section 12); they are *not* cascade-deleted with the session, and
  a hard delete of a session row is blocked while recording rows
  exist (matches the existing
  "`terminal_sessions` are NEVER deleted from the user UI" policy in
  `SPEC.md` → "Inventory lifecycle and destructive-action policy").
- **No terminal bytes in `audit_events`**. Lifecycle audit continues
  to use `audit_events`; the recording layer is a separate,
  owner-scoped surface. No row in `audit_events.payload` ever carries
  a chunk's bytes, a marker payload, or any string from a recording
  row.
- **Bounded blob size**: `payload` size is capped per row by
  `CHECK (byte_len <= ...)`; the chunk writer (Section 6.1) is the
  primary bound, the `CHECK` is defence-in-depth. The CHECK upper
  bound MUST cover the worst case: a single `Output` frame at the
  binary envelope's 1 MiB cap, plus envelope overhead from a future
  `encryption = 1` row (XChaCha20-Poly1305 nonce + tag + magic +
  version ≈ 41 bytes). Setting the CHECK at `2 MiB` is the
  recommended starting value — comfortable headroom over the 1 MiB
  frame cap, well below any TOAST anxiety. The CHECK MUST NOT be
  set to a value that would refuse a single envelope-encrypted
  1 MiB frame, because that would create a write surface that
  silently fails for legitimate workloads.
- **Owner scope is derived**, not duplicated. Recording rows do not
  carry their own `owner_id`; they inherit it via
  `terminal_sessions.owner_id`. Read APIs (Section 10) join through
  the session and apply `owner_id == user.user_id()` at the
  boundary, identical to existing `terminal_sessions` reads.

### 5.5 Marker payload rule

`terminal_recording_markers.payload` is JSONB and obeys the *spirit* of
the audit forbidden-substring rule (`AGENTS.md` → "Things to avoid",
`AUDIT_FORBIDDEN_SUBSTRINGS`):

- `started`, `attached`, `detached`, `reattached`, `closed` —
  `payload` is `{}` or an enum field naming the reason
  (`"client_requested"`, `"pty_teardown"`, etc., mirroring the
  `session_events` payload shape that already passes the audit
  sentinel test).
- `resized` — `{ "cols": <u16>, "rows": <u16> }`. No client_info,
  no remote_addr, no attachment id (those live on the attachment row).
- `replay_gap` — `{ "from_seq", "to_seq", "reason": "<enum>" }`.
  `reason` is one of `writer_overflow`, `writer_error`, `unknown`.
  No bytes, no error text.
- The marker payload **never** carries `attachment_id` /
  `remote_addr` / `client_info`. The attachment surface
  (`terminal_session_attachments`) already records those, and a
  recording marker is not the right place to denormalise them.

## 6. Storage format

### 6.1 Chunk size and seq continuity

- **Chunk target size**: a soft target of 64 KiB of payload bytes
  per chunk, OR 256 frames per chunk, OR a 1-second flush deadline,
  whichever fires first. The exact constants are the writer slice's
  to set (Section 13 step 3); these are the order-of-magnitude
  guidance.
- **Single most-recent chunk may overshoot the target** if a single
  `Output` frame exceeds it (binary envelope already caps a frame at
  1 MiB; the chunk writer must accept that bound and not silently
  fragment a single frame across chunks).
- **Seq continuity**: chunks are seq-aligned and contiguous *within a
  session*. The writer's invariant is `chunk[n].seq_start ==
  chunk[n-1].seq_end + 1`. A gap is materialised as a
  `replay_gap { from_seq, to_seq }` marker (Section 5.5), never as
  silent omission.
- **Single writer**: the orchestrator's PTY forwarder is the **sole**
  recording writer for a given session, mirroring the seq-assigner
  rule. Recording is a tee from the same forwarder that already
  writes the in-memory ring; there is no second writer.
- **Backpressure / failure mode**: the recording writer is
  bounded — a small async queue between the forwarder and the chunk
  flusher. If the queue overflows, the writer drops the *oldest
  un-flushed* chunks and emits a `replay_gap` marker covering the
  dropped seq range. The live wire is **never** blocked on recording;
  the live PTY path is the priority. Section 9 expands on the
  failure semantics.

### 6.2 Compression

- **v1**: no compression (`compression = 0`). PTY output already
  contains a lot of escape-sequence repetition; zstd would help but
  introduces a dictionary-versioning question and an extra dep. Ship
  the simple shape first; revisit when chunk volume forces it.
- **Future**: `compression = 1 = zstd`, per-chunk independent
  (no shared dictionary across chunks — that would couple chunks
  together and complicate retention). The migration adds a new value
  to the column's CHECK; existing rows stay `0`.

### 6.3 Encryption (envelope, decision deferred)

The vault crate (`crates/relayterm-vault/`) already encrypts SSH
identity private keys via XChaCha20-Poly1305 with a 32-byte master
key from typed config and a magic-prefix versioned envelope. The
*shape* generalises to recording:

- **Recording uses a SEPARATE master key** from the SSH-identity
  vault key. Compromising the recording key must not leak SSH
  identities, and vice versa. Both keys come from typed config; the
  config schema gains a new `recording_master_key` field (or
  equivalent) with the same "must be 32 bytes" validator the vault
  key already has.
- **v1 ships with encryption opt-in via a config flag**, *not*
  on-by-default. Reasoning: the chunk writer is a hot path and the
  XChaCha20-Poly1305 cost is non-trivial under heavy PTY output; the
  config must decide deliberately. The schema reserves
  `encryption = 1 = vault envelope`; a v1 deployment may run with
  `encryption = 0` if the operator has accepted the documented
  at-rest risk in their config.
- **Documented risk for `encryption = 0`**: the `payload` column
  contains plaintext PTY bytes. Anyone with read access to the DB
  (operator, backup, replica, future leaked dump) sees what the
  operator's terminal printed. The privacy posture (Section 7)
  spells this out as the headline warning the operator UI must
  surface before recording is enabled.
- **Envelope shape**: matches the vault's pattern — magic prefix,
  version byte, 24-byte XChaCha20 nonce, ciphertext, Poly1305 tag.
  The recording reader rejects an unknown magic / version with a
  typed error and never echoes the bytes.
- **`encryption` column is per-row** so a future migration that
  flips the operator's choice does not have to re-encrypt history
  in-place (history is read with whatever scheme it was written
  with).

### 6.4 Binary payload storage

- `BYTEA` in PostgreSQL. No `TOAST` tuning required at v1 chunk
  sizes; the migration slice should leave `STORAGE EXTENDED`
  (the `BYTEA` default) and revisit only if measured pain shows up.
- **No streaming chunk reads to clients** at the row level — the
  chunk reader hydrates a row, decrypts if needed, and emits its
  contents as a sequence of `Output { seq, data }` frames on the
  same WebSocket attach surface (Section 8.2). The wire path stays
  the same; the *source* is durable.

## 7. Privacy and security posture

Recording is the most privacy-sensitive surface RelayTerm has shipped.
The bar is "treat recordings like vault secrets, but acknowledge that
the operator has chosen to lengthen their lifetime."

- **Terminal output may contain secrets**. Anything an operator's
  shell printed — env-var dumps, decrypted file contents, API
  responses, tokens echoed by tooling, MOTD banners, `aws sts
  get-caller-identity` — is in scope of "what a recording contains."
  The recording layer does not attempt to detect or redact this.
- **Paste content may appear in terminal output** after the operator
  confirmed it. The paste safety policy (`SPEC.md` → "Production
  terminal paste safety") gates *what reaches the PTY*; once it
  reached the PTY, the shell can echo it, and the recording captures
  whatever the PTY emitted back. The recording subsystem is not the
  place to re-litigate paste safety.
- **Recordings MUST NOT appear in**:
  - any `audit_events.payload` field
    (`AUDIT_FORBIDDEN_SUBSTRINGS` is the backstop);
  - any `tracing::*` log line at any level (chunk writer Debug
    impls redact `payload` to `seq_start..=seq_end + len`, mirroring
    the in-memory `OutputFrame::Debug` rule);
  - any `panic!` / thrown `Error.message` / API error response;
  - any frontend `localStorage` / `sessionStorage` value (the
    existing `(session_id, last_seen_seq)` pointer remains the only
    durable client-side hint, and it carries no bytes);
  - any dashboard summary, session-list cell, or `data-*`
    attribute. The dashboard / list APIs return *counts and metadata*
    only; chunks are returned only by the dedicated read endpoints
    (Section 10) and only with explicit owner-scope.
- **Owner-scoped access only**. Recording reads go through the same
  `AuthenticatedUser` extractor as every other protected route. The
  cross-user existence-leak rule applies: a foreign session id
  collapses to a byte-identical 404 *before* any chunk row is
  considered.
- **No background third-party processing**. The recording subsystem
  does not stream chunks to any external service, search indexer, or
  notification surface. The DB is the only sink.
- **Operator UI warnings**. The future enable-recording UI MUST
  display a static warning: "Recording stores everything your
  terminal prints. Don't enable on shared deployments. At-rest
  encryption is opt-in (see config)." The download/export UI MUST
  display a second warning before yielding the bytes: "Exported
  transcripts contain everything the terminal printed, including
  any secrets the shell echoed." These strings are pinned in tests
  the same way the auth-check status strings are pinned today.
- **Default off**. Recording is disabled by default at the config
  layer. A self-hosted operator must opt in; the default config does
  not silently start writing PTY bytes to disk.
- **Conservative retention defaults** when enabled (Section 12). The
  default policy is "shortest reasonable window that makes the
  feature useful," not "infinite."

## 8. Replay semantics

### 8.1 Live attach (unchanged)

The existing in-memory ring continues to serve attach with
`last_seen_seq` inside its window. Nothing in this section changes
the wire shape of `Attach` / `ReplayStart` / `Output` /
`ReplayEnd` / `ReplayWindowLost`. The orchestrator continues to
assign seq from the PTY forwarder; the in-memory ring continues to
buffer the most recent `1024` frames OR `1 MiB`.

### 8.2 Durable replay (new)

When `Attach { last_seen_seq: Some(n) }` arrives and `n + 1` is
**older** than the in-memory ring's oldest retained frame, the
handler today emits `ReplayWindowLost` and continues live. With
recording enabled and chunks present, the handler instead:

1. Resolves `chunks_from = first chunk where seq_end >= n + 1`. If
   no such chunk exists, fall through to `ReplayWindowLost` —
   recording was either disabled at the time, swept by retention,
   or lost via `replay_gap`.
2. Emits `ReplayStart { from_seq: chunks_from.seq_start.max(n + 1),
   to_seq: latest_seq_at_query }` exactly once (same wire shape as
   today).
3. Streams the chunk stream. Each chunk decrypts (if
   `encryption != 0`) and decompresses (if `compression != 0`)
   server-side, then emits ONE `Output { seq, data }` frame per
   chunk, where `seq = chunk.seq_end` and `data` is the chunk's
   plaintext payload. Chunks whose `seq_end <= n` are skipped
   entirely; the chunk that straddles `n` (i.e.
   `seq_start <= n < seq_end`) is emitted in full — the renderer's
   VT parser already handles starting in the middle of an
   escape-sequence run by treating the prefix bytes as opaque
   continuation, the same way the in-memory ring's first replayed
   frame is handled today. (A future slice MAY refine this to
   per-frame boundaries if a measured fidelity issue emerges, but
   the v1 contract is "chunk-granular replay, renderer handles
   continuation.")
4. When the chunk stream runs out, *bridges into the in-memory
   ring* at the first ring frame with `seq > durable.latest_seq`
   and continues until caught up to live.
5. Emits `ReplayEnd { latest_seq }` and resumes live fanout. The
   `min_live_seq` floor rule (`SPEC.md` → "Replay handshake on
   attach") still applies — the handler tracks the highest replayed
   seq and drops live broadcast frames at or below it to avoid
   double-delivery.

### 8.3 Durable gaps

If the chunk stream contains a `replay_gap` marker between
`chunks_from` and `latest_seq`, the handler emits a
`ReplayWindowLost { requested_seq: n, oldest_available_seq: <gap
boundary>, latest_seq }` and stops the durable replay. The renderer
treats this exactly as today: reset the grid, continue live. **The
handler never fakes continuity across a gap** — a gap is a real
loss of fidelity and the renderer is told so.

A future slice may add a finer-grained `ReplayPartial` shape that
lets the renderer keep what it has and only reset across the gap;
that is not in scope for v1 of recording. The v1 contract is
"either continuous catch-up or a clean window-lost."

### 8.4 Closed-session replay

A `closed` session has no live PTY and no in-memory ring. A
replay-only attach (Section 10 endpoint) reads the chunks from
`seq_start = 1` (or a caller-supplied `from_seq`), emits
`ReplayStart` → chunked `Output` → `ReplayEnd { latest_seq:
<final_seq_in_recording> }`, and then closes. There is no live
fanout; the wire close is the signal that the recording has been
fully delivered.

The frontend uses this to render a static transcript view. The
existing `TerminalSessionClient` state machine extends with a
`replay_only` *terminal* state (final after `ReplayEnd` on a
closed-session attach) — the renderer remains the standard
xterm baseline; only the lifecycle differs.

### 8.5 Backend restart interaction (recap; details in Section 9)

A restart drops the live PTY, the broadcast channel, and the
in-memory ring. Recording chunks survive (they're in Postgres). The
wire sees: any in-flight WS gets a transport drop; reattach against
the now-orphaned `active` row resolves to the closed-session replay
path above (after Section 9.2 reconciles the row).

## 9. Backend restart recovery

### 9.1 What does NOT recover

- The live `russh::Channel`. Once the process exits, the SSH
  transport is gone. The remote shell may still be running on the
  target (it does not know the server died), but the orchestrator
  has no handle to re-attach to it.
- The in-memory replay ring (already documented as non-durable).
- The bounded broadcast channel feeding live attachments.
- Pending detached-TTL `JoinHandle` timers. These were anchored on
  `tokio::sleep`; restart cancels them implicitly.

### 9.2 What DOES recover

- All `terminal_sessions` rows (existing).
- All `session_events` rows (existing).
- All chunk rows + marker rows (new).
- The owner-scoped read APIs continue to work.

### 9.3 Startup reconciliation policy

On startup the orchestrator runs a small reconciliation pass for
its own metadata before accepting requests:

1. Scan `terminal_sessions WHERE status IN ('starting', 'active',
   'detached')`. These rows describe sessions whose runtime entry
   was lost across the restart.
2. For each such row, append a `closed { reason: "backend_restart",
   category: "process_lost" }` lifecycle event (`session_events`)
   AND transition the row to `closed`. If recording is enabled and
   any chunk row exists for the session, also write a `closed`
   recording marker at the highest persisted seq.
3. **Reconciliation writes `session_events` only, NOT
   `audit_events`.** Rationale: the existing wired close path
   (`POST /api/v1/terminal-sessions/:id/close`) writes a `closed`
   `session_events` row but does not write an `audit_events` row —
   `audit_events` is reserved for security-relevant outcomes
   (auth, vault access, profile/identity mutations, host-key
   decisions) and explicit lifecycle moves audited under their own
   kind (`server_profile_*`). Restart reconciliation is operational
   bookkeeping, not a security event, and matches the current
   close-path audit shape exactly. If a future slice decides
   restart-reconciliation IS audit-worthy, it adds a dedicated
   `terminal_session_reconciled` audit kind via the same migration
   pattern as `recording_purged` (Section 13 step 8) — but that is
   NOT in scope for the recording slices.
4. The reconciliation pass does NOT delete chunk rows. The
   recording remains readable via the closed-session replay path.
5. Reconciliation is idempotent: a second startup that finds no
   pre-restart non-`closed` rows is a no-op.

This is the first step where session lifecycle and recording
lifecycle interact, and it must be wired together — a row that gets
reconciled to `closed` without its recording marker would leave the
read path showing a session that "ended" with no end marker.

### 9.4 Future work: live PTY persistence

Long-running tmux/screen-style detached-PTY persistence beyond
`DETACHED_LIVE_PTY_TTL` is already named as future work in
`SPEC.md` → "Output sequence + in-memory replay buffer contract".
Recording does not deliver this — recording captures *display
history*, not the *live PTY*. A future slice that wants live PTY
recovery across a restart needs to externalise the russh session
state itself (a different problem from recording bytes).

## 10. API design sketch

All endpoints are owner-scoped via `AuthenticatedUser`; foreign
session ids collapse to byte-identical 404 *before* any chunk row
is touched. State-changing routes (none in this section yet, but
future enable/disable endpoints) follow the
`CsrfGuard` rule from `AGENTS.md`.

### 10.1 `GET /api/v1/terminal-sessions/:id/recording/metadata`

Returns metadata only: `{ "session_id", "recording_enabled",
"first_seq", "last_seq", "chunk_count", "marker_count",
"first_marker_at", "last_marker_at", "encryption", "compression" }`.
No bytes. Returns `404` for foreign or unknown sessions, `404` for
sessions that have no recording rows (recording disabled), `200`
with metadata otherwise.

### 10.2 `GET /api/v1/terminal-sessions/:id/recording/chunks?from_seq=...&limit=...`

Streams or pages chunked output for a closed session (or a session
with no live PTY runtime). The wire shape is **per-chunk, not
per-frame**: each entry in the response array is one `chunk` row,
namely `{ seq_start, seq_end, byte_len, data_b64, encryption,
compression }`. This avoids forcing the schema to store intra-chunk
frame boundaries (Section 5.1 deliberately stores concatenated
payload bytes only) and avoids forcing the REST handler to re-parse
chunk bytes back into per-frame `Output { seq, data }` shape on the
hot read path.

The `data_b64` field uses the same base64 codec as the legacy JSON
`Output` shape (`output_data_encode/decode` in `relayterm-protocol`).
Binary `RTB1` framing is **not** used on this REST surface — REST
clients should not be forced to parse the binary envelope. `data_b64`
is the chunk's *post-decryption, post-decompression* plaintext bytes
when the row's `encryption`/`compression` are `0`; for non-zero
schemes the handler MUST decrypt/decompress server-side and emit
plaintext bytes — the wire MUST NOT carry envelope ciphertext, since
the recording master key never crosses the API boundary.

The renderer treats each `data_b64` payload as a contiguous slice of
PTY output bytes and feeds it directly into `renderer.write(bytes)` —
the renderer's existing VT parser handles cross-chunk escape-sequence
boundaries the same way it already handles cross-frame boundaries on
the live wire.

`from_seq` defaults to `1`. `limit` is bounded server-side
(suggested max 32 chunks per response; the client paginates with
the highest-seen `seq_end + 1` as the next `from_seq`). Foreign /
unknown / no-recording → `404`.

### 10.3 WebSocket replay (existing surface, extended)

The existing `GET /api/v1/terminal-sessions/:id/ws` upgrade
continues to be the live attach surface. Sections 8.2 and 8.4
describe how the handler extends to source replayed frames from
chunks when the bookmark predates the in-memory ring, and how
closed-session attach drives a replay-only flow before the wire
closes. **No new wire variant is required** — `ReplayStart` /
`Output` / `ReplayEnd` / `ReplayWindowLost` already cover it.

### 10.4 What MUST NOT appear

- Any chunk byte material on `GET /api/v1/terminal-sessions`
  (the list endpoint).
- Any chunk byte material on the dashboard summary endpoint
  (`SPEC.md` → "Production dashboard summary").
- Any chunk byte material on any session-detail panel that is not
  the explicit replay surface.
- Any chunk byte material in any `error` response body, any
  `tracing::*` log line, or any `audit_events.payload`.
- Any "search recordings for X" surface in v1. Full-text search
  over PTY bytes is a privacy nightmare and is explicitly not in
  scope.

## 11. Frontend UX sketch

This section names the future production-shell surfaces that
recording will eventually grow. None of them ship in the design
slice; they are the pen-and-paper view of what Section 13's later
steps will land.

- **Production terminal sessions list** (`SPEC.md` → "Production
  terminal sessions list/status UI") gains a small "recording"
  indicator per row (`recording: enabled | disabled | n/a`),
  source-of-truth from the metadata endpoint. The cell is
  metadata-only; no preview, no byte count exposed beyond
  "available".
- **Closed-session detail** gets a "Replay recording" affordance
  when `recording_enabled = true` and `chunk_count > 0`. The
  affordance opens a separate **replay-only** view that mounts the
  standard `XtermRenderer` but binds it to a replay-only
  `TerminalSessionClient` (Section 8.4). The view's title and
  badge make the read-only nature visible — no `input` is allowed,
  the `disconnect`/`close` controls are absent, the only operator
  affordance is "rewind / play / pause" (later) or "scroll
  through" (v1).
- **Live reconnect** uses the durable path transparently when a
  bookmark predates the in-memory ring (Section 8.2). The user-
  facing change from today is "reconnect now succeeds in cases that
  used to surface `ReplayWindowLost`." When the chunk stream itself
  has a `replay_gap`, the renderer surfaces "Some history was lost
  during reconnect. The grid has been reset." — the existing
  reset-on-window-lost copy already in the lab.
- **Clear separation of live vs replay surfaces**. The replay
  viewer is a different view, not a re-skin of the live terminal.
  Production code MUST NOT make the live `ProductionTerminal`
  component swap its data source mid-flight; a closed session and
  a live session are visibly different surfaces.
- **No recording bytes in session lists / dashboards / activity
  feeds**. The dashboard recent-activity surface (`SPEC.md` →
  "Dashboard recent activity") continues to read `audit_events`
  only and never joins through the recording tables.
- **Warnings**:
  - Before enabling recording for a `server_profile` (future
    config-driven knob): a static warning naming what gets
    captured and the at-rest encryption posture.
  - Before downloading/exporting a recording (future,
    out-of-scope-for-v1): a second static warning naming the
    secrets risk.
- **Mobile / Tauri**. Mobile-specific replay UX is explicitly
  future work (Section 3 non-goal). The Tauri shells inherit the
  web replay viewer until a mobile-specific surface lands.

## 12. Retention and cleanup

The recording corpus grows monotonically until something cleans it
up. The policy below is what the future cleanup slice MUST
implement; the v1 recording slice ships with manual operator
control only (config flag + DB-level housekeeping).

- **Default off**. Recording is disabled by default at the config
  layer. An operator opts in deliberately.
- **Per-session retention** (when enabled): default **30 days**
  from `terminal_sessions.closed_at`. Sessions with no `closed_at`
  (still live) are retained indefinitely while live; once
  `closed_at` is stamped (or backfilled by Section 9.3
  reconciliation), the retention clock starts.
- **Per-session byte cap** (when enabled): default **64 MiB** of
  payload bytes per session. The chunk writer (Section 6.1) enforces
  the cap inline by emitting a `replay_gap { reason:
  "byte_cap_reached" }` marker when adding the next chunk would
  exceed the cap, then dropping further frames *for recording only*
  — the live wire is never affected. (Whether to overwrite-front or
  drop-tail is a writer-slice decision; this design recommends
  drop-tail because it keeps the chunk stream contiguous up to the
  gap and makes the cap visible to a replay viewer.)
- **Cleanup worker** (later slice): a periodic background task
  scans `terminal_sessions WHERE closed_at < now() -
  recording_retention_days` and deletes the matching chunk and
  marker rows. Per the FK rule (Section 5.4), the session row
  itself is *not* deleted — historical session metadata survives
  retention sweeps. The sweep writes one `audit_events` row per
  swept session: `kind = "recording_purged"`, `payload = {
  "session_id", "chunk_count", "marker_count" }` — counts and ids
  only, no bytes. (This audit kind requires a new migration when
  the cleanup slice lands.)
- **Admin retention UI**: out of scope for v1 of recording. Config
  + the cleanup worker is the v1 surface; an admin UI to view
  per-session retention overrides comes later, behind the same
  multi-user / RBAC story RelayTerm doesn't have today.

## 13. Implementation order

Each step is its own slice, each ships behind config flags, each is
fully reviewable in isolation. **No step ships unless the prior
step is green AND has owner-facing UX or operator-facing docs that
make its state observable.** Incremental landings keep the privacy
posture (Section 7) defensible at every stop.

1. **This design doc + config flags** (split: 1a = doc only,
   1b = config plumb).
   - **Step 1a (landed)**: this design doc landed by itself.
     No code, no config changes, no migrations, no UI. The doc
     is the binding contract every later step references.
   - **Step 1b (landed)**: typed `[terminal_recording]` section
     wired into `apps/backend/src/config.rs` (TOML + env overrides,
     redaction-aware `Debug`), validated at boot in
     `apps/backend/src/main.rs` immediately after `validate_auth`.
     Defaults: `enabled = false`, `retention_days = 30`,
     `max_bytes_per_session = 64 MiB`, `chunk_target_bytes = 64 KiB`,
     `chunk_hard_cap_bytes = 2 MiB`, `encryption.mode = disabled`,
     `compression.mode = none`. Production envelope: `enabled = true`
     requires `encryption.mode = required` AND exactly one of
     `master_key_b64` / `master_key_file`; the recording master key
     MUST be a SEPARATE secret from `vault.master_key_b64` /
     `vault.master_key_file` — the validator rejects equal-source
     pairs statically (mixed sources, e.g. b64 vs. file path, are an
     acknowledged gap and a future runtime check after key load).
     Numeric bounds are enforced unconditionally:
     `chunk_target_bytes <= chunk_hard_cap_bytes`;
     `chunk_hard_cap_bytes >= 1 MiB + envelope budget`;
     `max_bytes_per_session >= chunk_hard_cap_bytes`;
     `retention_days` in `1..=3650`;
     `max_bytes_per_session <= 1 TiB`. Env names follow the existing
     double-underscore-as-section convention:
     `RELAYTERM_TERMINAL_RECORDING__ENABLED`,
     `RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS`,
     `RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION`,
     `RELAYTERM_TERMINAL_RECORDING__CHUNK_TARGET_BYTES`,
     `RELAYTERM_TERMINAL_RECORDING__CHUNK_HARD_CAP_BYTES`,
     `RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE`,
     `RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_B64`,
     `RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_FILE`,
     `RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE`. The example
     TOML files at `docs/config-examples/relayterm.{production,dev}.example.toml`
     show the disabled-by-default shape and enable-time guidance.
   - Step 1b ships **no DB writes**. The repository in step 2 is
     the first slice that creates tables. Flipping
     `terminal_recording.enabled = true` today changes no runtime
     behaviour — it only causes production validation to require a
     separate master key.
2. **Schema + repository for output chunks and markers** (landed).
   - Two sqlx migrations:
     `20260502000018_terminal_recording_chunks.sql` and
     `20260502000019_terminal_recording_markers.sql`. Both tables
     reference `terminal_sessions(id) ON DELETE RESTRICT` per
     Section 5.4. The chunk table enforces
     `seq_start >= 1`, `seq_end >= seq_start`,
     `byte_len > 0 AND byte_len <= 2 MiB`,
     `octet_length(payload) = byte_len`,
     `encryption IN ('none')`, `compression IN ('none')`, and
     `UNIQUE (terminal_session_id, seq_start)`. The marker table
     enforces `seq >= 0`, `kind` in the seven-element set
     (`started`, `attached`, `detached`, `reattached`, `resized`,
     `closed`, `replay_gap`), the seq=0-only-for-`started`
     constraint per Section 5.2, and a JSON-object check on
     `payload`. Indexes: chunks on
     `(terminal_session_id, seq_start)`; markers on
     `(terminal_session_id, seq, created_at)`.
   - Domain types in `crates/relayterm-core/src/terminal_recording.rs`:
     `TerminalRecordingChunk` (manual `Debug` redacts `payload` to
     length-only; deliberately NOT `Serialize`), `TerminalRecordingMarker`
     (metadata-only `payload`, `Debug` derived),
     `TerminalRecordingMarkerKind`,
     `TerminalRecordingPayloadEncryption`, and
     `TerminalRecordingCompression` (each enum with
     `as_str` / `from_str_tag`, mirroring the schema CHECKs).
   - Repository trait `TerminalRecordingRepository` in
     `relayterm_core::repository` (session-scoped; owner-scoping
     happens at the API layer) with bounded-input helpers:
     `append_chunk`, `append_marker`, `list_chunks(session, from_seq, limit)`,
     `list_markers(session, from_seq, limit)`. `CreateTerminalRecordingChunk`
     redacts `payload` in `Debug`; `CreateTerminalRecordingMarker`
     stores metadata-only JSON.
   - Postgres impl in `crates/relayterm-db/src/repositories/terminal_recording.rs`,
     reachable as `Db::terminal_recordings()`. Lists clamp `limit`
     to `[1, 1024]` defence-in-depth on top of any future API
     pagination cap.
   - Unit tests in `relayterm-core` cover marker-kind round-trip,
     unknown-tag rejection, allows-seq-zero-only-for-started,
     chunk Debug redaction, create-chunk-input Debug redaction,
     and marker Debug formatting. Postgres-tests in
     `crates/relayterm-db/tests/repositories.rs` cover insert +
     list happy path, ordered listing, `from_seq` filtering,
     duplicate `seq_start` Conflict, byte_len/payload mismatch
     rejection, byte_len=0 rejection, seq_start=0 rejection,
     unknown-session FK rejection, error-and-Debug redaction
     against a sentinel byte string, marker round-trip,
     `started`-allows-seq=0, seq=0-rejected-for-other-kinds,
     marker filtering, and session-scoped isolation between users.
   - **No writes from the orchestrator yet**. Repository is dead
     code for now; the next slice wires it. No durable replay API,
     no replay-only frontend, no startup reconciliation, no
     retention worker, no encryption / compression implementation,
     no `recording_purged` audit kind, no VT snapshot observer,
     and no export endpoint exist yet.
3. **Orchestrator writes chunks + markers**.
   - Tee from the PTY forwarder into a bounded async chunk
     writer; flush on size / frame-count / time deadline.
   - Markers written for `started`, `attached`, `detached`,
     `reattached`, `resized`, `closed` at the seq the event
     observed.
   - Backpressure: drop oldest unflushed chunks on overflow,
     emit `replay_gap`. Live wire never blocks.
   - Tests: chunk continuity invariant (`chunk[n].seq_start ==
     chunk[n-1].seq_end + 1`); replay_gap on simulated overflow;
     redaction sentinel test (no chunk bytes in any
     `tracing::*` line, no chunk bytes in any `audit_events`
     row written by this slice).
4. **Durable replay read API**.
   - The two HTTP endpoints in Section 10.1 / 10.2.
   - Owner-scope tests (foreign session 404), no-recording 404
     tests, bounded `limit` tests.
   - Redaction sentinel tests against the API response bodies.
5. **Frontend replay viewer for closed sessions**.
   - New production view under `apps/web/src/lib/app/views/`,
     wired through `AppViewId` / `NAV_ITEMS`. Mounts
     `XtermRenderer` against a replay-only client.
   - Honest "read-only", "may contain secrets" copy on first
     open.
   - **Breaking contract change** — adding the `replay_only`
     variant to `TerminalSessionState` in
     `@relayterm/terminal-core/src/client.ts` is a public-API
     change for the package. Every exhaustive `switch` site over
     `TerminalSessionState` (production app shell, dev lab,
     diagnostics panel, tests) MUST be updated in the same
     commit that adds the variant. Treat this with the same care
     as adding a `ServerMsg` variant in `relayterm_protocol`:
     bump the package version, audit every `switch` site, add a
     compile-time exhaustiveness assertion test if one does not
     already exist. Renderer adapters are unaffected — the
     `TerminalRenderer` interface is renderer-state-only and
     does not see `TerminalSessionState`.
   - Sentinel test asserting no chunk byte material reaches
     `localStorage`/`sessionStorage` and that the parsed DTO
     never carries a `recording_master_key` /
     `encryption_master_key` / similar field.
6. **Startup reconciliation for active sessions after crash**.
   - Section 9.3 policy. Idempotent. Audited via existing
     lifecycle audit kinds (no new audit kind required for the
     row close itself; the `closed` lifecycle event already
     covers it). One recording marker per reconciled session if
     recording rows exist.
7. **VT snapshot observer** (optimisation; only after 1–6 land
   and there is operator demand for fast-forward).
   - Section 5.3 schema + a `terminal-vt` writer that
     checkpoints the libghostty-vt grid at intervals.
   - Replay path (Section 8) chooses the nearest snapshot ≤
     `from_seq` and replays only the chunks past it.
8. **Retention cleanup job**.
   - Section 12. Configurable retention window.
   - The new `recording_purged` audit kind is added with the
     full audit-kind extension protocol from `SPEC.md` →
     "Inventory lifecycle and destructive-action policy" →
     "Audit-event expectations":
     1. Dedicated sqlx migration that extends the
        `audit_events_kind_chk` CHECK constraint to include
        `'recording_purged'` (migration adds, never replaces;
        existing kinds stay).
     2. `AuditEventKind` Rust enum gains the variant; serde
        snake_case rename pins the wire tag to `"recording_purged"`.
     3. Unit test asserts the wire tag round-trips exactly
        `"recording_purged"` (matches the existing
        `server_profile_disabled` / `server_profile_enabled` /
        `password_changed` test pattern).
     4. Sentinel test asserts the cleanup worker's audit row
        passes `AUDIT_FORBIDDEN_SUBSTRINGS` against a synthetic
        PTY workload — counts and ids only, never chunk bytes,
        never marker payloads, never error text.
     5. `.sqlx/` offline metadata is regenerated and committed.
9. **Optional encryption / compression / export controls**.
   - Flip `recording.encryption_required: bool` to a third config
     state (`required` → refuse to start without a key AND refuse
     to write `encryption = 0` rows). Add `compression = 1 = zstd`
     to the chunk-writer code path. Add an explicit operator
     export endpoint with the second warning copy.

## 14. Tests to require (future slices)

The implementation slices above MUST add the following classes of
test. This list is the contract a future code-reviewer enforces;
none of these tests are written in this design slice.

- **Chunk sequence continuity**. After a synthetic PTY workload,
  every adjacent chunk pair satisfies `chunk[n].seq_start ==
  chunk[n-1].seq_end + 1`, OR is bracketed by a `replay_gap`
  marker that exactly covers the missing span.
- **Redaction / no audit leakage**. Sentinel-string tests
  modeled on `crates/relayterm-api/tests/api.rs`'s
  `AUDIT_FORBIDDEN_SUBSTRINGS` pattern. Every recording row's
  `Debug` output, every API response body that mentions a
  recording, every `tracing::*` line emitted by the chunk writer,
  and every `audit_events.payload` row written by any recording
  slice MUST NOT contain any sentinel string injected through
  the synthetic PTY workload.
- **Owner-scoped access**. A foreign session id on every
  recording endpoint returns a byte-identical 404. A user with
  zero recording rows for an owned session returns a documented
  404 (recording disabled / no rows).
- **Replay gap behavior**. A synthetic overflow that triggers
  `replay_gap` produces `ReplayWindowLost` on a reattach
  bracketing the gap, AND the renderer's reset path is the
  observed wire response — never silent continuity.
- **Backend restart reconciliation**. A test process kill +
  restart with one `active` row and recording rows present
  produces (a) a `closed` lifecycle event with `reason =
  backend_restart`, (b) a `closed` recording marker at the
  highest persisted seq, and (c) idempotency on a second
  restart.
- **Retention cleanup**. The cleanup worker, run against a
  fixture with sessions older than the retention window, deletes
  exactly the chunk and marker rows for those sessions, leaves
  the `terminal_sessions` rows in place, and writes one
  `recording_purged` audit row per swept session.
- **No input recording by default**. With recording enabled and
  `record_input` configured to its default (off, or unset), zero
  rows in any recording table reference an `input` payload.
  Sentinel test: the synthetic workload sends a unique input
  string; that string MUST NOT appear in any chunk's decrypted
  payload, any marker payload, any audit row, any tracing line.
- **Recording disabled means no chunks**. With `recording.enabled
  = false`, a full PTY session writes zero rows to
  `terminal_recording_chunks` and zero rows to
  `terminal_recording_markers`. The metadata endpoint returns
  404. The replay endpoint returns 404. The replay viewer's
  client-side guard refuses to mount.

---

## Appendix: cross-references

- `SPEC.md` → "Live SSH PTY bridge contract"
- `SPEC.md` → "Output sequence + in-memory replay buffer contract"
- `SPEC.md` → "Detached-session TTL contract"
- `SPEC.md` → "Inventory lifecycle and destructive-action policy"
- `SPEC.md` → "Production terminal paste safety"
- `SPEC.md` → "Production active terminal local recovery"
- `AGENTS.md` → "Things to avoid" (audit forbidden substrings,
  CsrfGuard rule, AuthenticatedUser rule, paste safety rule)
- `crates/relayterm-terminal/src/replay.rs` (in-memory ring)
- `crates/relayterm-terminal/src/manager.rs` (DETACHED_LIVE_PTY_TTL
  constant, runtime registry)
- `crates/relayterm-vault/` (envelope shape recording will mirror
  with a separate master key)
- `crates/relayterm-api/tests/api.rs` →
  `AUDIT_FORBIDDEN_SUBSTRINGS` (sentinel pattern recording must
  inherit)
