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
against a runaway chunk row. The chunk writer
(`crates/relayterm-terminal/src/recording.rs`) is the primary bound
(`chunk_hard_cap_bytes`, default 2 MiB); the CHECK is the hard upper
bound. 2 MiB covers the
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
  `reason` is one of `writer_overflow`, `writer_error`,
  `frame_oversized` (matching the constants in the
  `replay_gap_reason` module in
  `crates/relayterm-terminal/src/recording.rs`).
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

**Status (this slice).** Terminal-session reconciliation AND the
closed-recording-marker reconciliation have landed. The
reconciliation pass runs at boot AFTER the database pool is ready
and BEFORE the listener binds (and BEFORE the
`TerminalSessionManager` is constructed, so its in-memory registry
starts from a clean slate). Live PTY recovery across the restart
is intentionally NOT in scope (Section 9.4) — the row is closed,
the recording is preserved, the replay viewer can render the
session up to the last persisted seq.

On startup the orchestrator runs a small reconciliation pass for
its own metadata before accepting requests:

1. Scan `terminal_sessions WHERE status IN ('starting', 'active',
   'detached')`. These rows describe sessions whose runtime entry
   was lost across the restart.
2. For each such row, append a `closed` lifecycle event
   (`session_events`) with payload `{ "reason":
   "startup_reconciliation", "previous_status": <starting | active
   | detached>, "reconciled_at": <ISO 8601 UTC> }` AND transition
   the row to `closed` (`closed_at = reconciled_at`,
   `last_seen_at = NOW()`). The status update AND the
   session_event row are committed in the same database
   transaction; a partial reconciliation that closes a row without
   leaving an audit trail is not possible.
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
   NOT in scope for the recording slices. The closed-marker write
   added below follows the same rule: zero `audit_events`.
4. The reconciliation pass does NOT delete chunk rows. The
   recording remains readable via the closed-session replay path.
5. Inside the same transaction, for any reconciled session that
   has at least one chunk row AND does not already have an
   equivalent `(kind = closed, seq = MAX(seq_end))` marker, append
   one `terminal_recording_markers` row:
   - `kind`: `closed`
   - `seq`: `MAX(seq_end)` across the session's chunks (the highest
     persisted output seq). Sessions with zero chunks are skipped —
     no marker, no SQL writes beyond the existing session-status
     and `session_events` updates.
   - `payload`: `{ "reason": "startup_reconciliation",
     "previous_status": <prior>, "reconciled_at": <ISO 8601 UTC> }`
     — public metadata only, built field-by-field, mirroring the
     `session_events` payload. NEVER chunk bytes, NEVER
     `client_info`, NEVER peer banners.
   - The replay viewer can now render "session ended at seq N due
     to backend restart" instead of the previous "trailing chunk
     + no end marker" shape.
6. Reconciliation is idempotent on three axes:
   - The outer scan only iterates non-closed candidates, so a
     second startup that finds no pre-restart non-`closed` rows is
     a no-op (no `session_events` appended, no marker written, no
     rows touched).
   - The marker insert uses `ON CONFLICT DO NOTHING` against the
     partial unique index
     `terminal_recording_markers_session_closed_seq_uidx` on
     `(terminal_session_id, seq) WHERE kind = 'closed'`. A partial
     earlier run, an operator-written marker at the same seq, or
     two racing writers all collapse to a single row at the
     database — the idempotency guarantee is a schema invariant,
     not an application convention. Pre-existing markers are
     preserved untouched.
   - Sessions without any chunk row never reach the marker insert.
7. Live PTY recovery is NOT in scope. The `russh::Channel`,
   broadcast fanout, and replay ring buffer of an orphaned session
   are unrecoverable across the restart (Section 9.1). The session
   row is closed; the durable recording is the artefact that
   survives.

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

**Status (this slice).** The read-API foundation has landed —
metadata, chunks, and markers endpoints are wired in
`crates/relayterm-api/src/routes/v1/terminal_recordings.rs`. They
are owner-scoped, write zero audit rows, and surface chunk bytes
as base64 only. Behaviour caveats versus the original sketch are
called out inline below.

### 10.1 `GET /api/v1/terminal-sessions/:id/recording/metadata`

Returns aggregate metadata across the session's chunk and marker
rows. The response shape is:

```json
{
  "terminal_session_id": "<uuid>",
  "has_recording": true,
  "chunk_count": 0,
  "marker_count": 0,
  "first_seq": null,
  "last_seq": null,
  "first_recorded_at": null,
  "last_recorded_at": null
}
```

Where `has_recording` is `true` iff at least one chunk OR marker
row exists for the session. `first_seq` / `last_seq` are derived
from chunks only (`MIN(seq_start)` / `MAX(seq_end)`); a session
that has only a `started` marker reports `chunk_count = 0`,
`first_seq = null`, but `has_recording = true`. Foreign / unknown
sessions → `404 terminal_session not found`. **Note** versus the
original sketch: an empty-recording session (no chunks AND no
markers) returns `200` with `has_recording = false` rather than
`404` — this lets a future UI distinguish "session exists, never
recorded" from "session does not exist." Reads write zero audit
rows. Recording disabled / never enabled is observable through
`has_recording = false`.

### 10.2 `GET /api/v1/terminal-sessions/:id/recording/chunks?from_seq=...&limit=...`

Streams or pages chunked output for a closed session (or a session
with no live PTY runtime). The wire shape is **per-chunk, not
per-frame**: each entry in the response array is one `chunk` row,
namely `{ seq_start, seq_end, byte_len, data_b64, encryption,
compression, created_at }`. This avoids forcing the schema to store
intra-chunk frame boundaries (Section 5.1 deliberately stores
concatenated payload bytes only) and avoids forcing the REST
handler to re-parse chunk bytes back into per-frame `Output { seq,
data }` shape on the hot read path.

The `data_b64` field uses the same base64 codec as the legacy JSON
`Output` shape (`output_data_encode/decode` in `relayterm-protocol`).
Binary `RTB1` framing is **not** used on this REST surface — REST
clients should not be forced to parse the binary envelope. In v1
(`encryption = 'none'`, `compression = 'none'`) `data_b64` is the
chunk's plaintext bytes as persisted. When a future
`encryption = 'recording_v1'` row lands, the handler MUST
decrypt/decompress server-side and emit plaintext bytes — the wire
MUST NOT carry envelope ciphertext, since the recording master key
never crosses the API boundary. The current implementation will
return rows with `encryption != 'none'` opaquely (still as base64);
that path is unreachable today because the writer only emits
`'none'`.

The renderer treats each `data_b64` payload as a contiguous slice of
PTY output bytes and feeds it directly into `renderer.write(bytes)` —
the renderer's existing VT parser handles cross-chunk escape-sequence
boundaries the same way it already handles cross-frame boundaries on
the live wire.

`from_seq` defaults to `1`; negative values are rejected as `400
invalid_input` (the wire body does NOT echo the offending value).
`limit` clamps to `1..=1024` at the API layer; the repository adds
the same `1024` ceiling underneath as defence-in-depth. Default
page size is `256`. Foreign / unknown sessions → `404`. An empty
recording returns `200 []` (NOT `404`) so callers can distinguish
"no chunks yet" from "no such session."

### 10.3 `GET /api/v1/terminal-sessions/:id/recording/markers?from_seq=...&limit=...`

Returns marker rows ordered by `(seq ASC, created_at ASC)`. The
response shape per item is `{ kind, seq, payload, created_at }`.
`kind` is one of the canonical tags (`started`, `attached`,
`detached`, `reattached`, `resized`, `closed`, `replay_gap`).
`payload` is metadata-only by writer contract — counts, dims,
reason codes — never PTY bytes.

`from_seq` defaults to `0` for markers (the `started` marker rides
at `seq = 0`); negative values are rejected as `400 invalid_input`.
`limit` follows the same clamp rules as chunks. Foreign / unknown →
`404`. Empty list returns `200 []`.

### 10.4 WebSocket replay (existing surface, extended)

The existing `GET /api/v1/terminal-sessions/:id/ws` upgrade
continues to be the live attach surface. Sections 8.2 and 8.4
describe how the handler extends to source replayed frames from
chunks when the bookmark predates the in-memory ring, and how
closed-session attach drives a replay-only flow before the wire
closes. **No new wire variant is required** — `ReplayStart` /
`Output` / `ReplayEnd` / `ReplayWindowLost` already cover it.

### 10.5 What MUST NOT appear

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
up. The recording-writer foundation (Section 6, Section 9.3,
implementation step 3) already persists chunks and markers; the
read API (Section 10) and the replay viewer (step 5) already render
them. There is **no purge surface yet** — neither a cleanup worker,
a `recording_purged` audit kind, nor any DELETE path. The corpus
is operator-managed by hand today (or by manual `DELETE` against
the schema), and it grows until something cleans it up.

This section is the binding contract for the future retention
slice (implementation step 8). The slice MUST land exactly the
shape below; any drift goes through this document first, not
through code.

### 12.1 Current state (as of this doc slice)

What already exists that retention rests on:

- `terminal_recording_chunks` and `terminal_recording_markers`
  schemas (Section 5; migrations
  `20260502000018_terminal_recording_chunks.sql` and
  `20260502000019_terminal_recording_markers.sql`). FKs to
  `terminal_sessions(id)` are `ON DELETE RESTRICT` — recording
  rows are NEVER cascade-deleted with their session row.
- The output-only chunk + marker writer (Section 6, Section 9.3).
  The writer emits `started`, `closed`, `resized`, and
  `replay_gap` markers; `attached`/`detached`/`reattached` are
  deferred to a follow-up.
- The owner-scoped read API (Section 10): three GET routes under
  `/api/v1/terminal-sessions/:id/recording/{metadata,chunks,markers}`.
- The frontend replay viewer (step 5).
- Startup reconciliation (Section 9.3) that closes orphaned
  `terminal_sessions` rows AND writes a `closed` recording marker
  at `MAX(seq_end)` for any reconciled session that has at least
  one chunk row. The reconciliation pass writes ZERO
  `audit_events` rows; it is operational bookkeeping, not a
  destructive action.

What does NOT exist yet and is what step 8 will land:

- No cleanup worker (neither startup-only nor periodic).
- No `recording_purged` audit kind in
  `audit_events_kind_chk` and no matching `AuditEventKind`
  variant.
- No `delete_for_session` (or equivalent) repository method on
  `TerminalRecordingRepository`.
- No retention-related cleanup config block; only the policy
  inputs (`terminal_recording.retention_days`,
  `terminal_recording.max_bytes_per_session`) are accepted and
  validated at boot.

### 12.2 Retention policy

**Eligibility is keyed on `terminal_sessions.closed_at`, not on
chunk `created_at`.** Eligibility for purge is per-session, not
per-chunk. A session's recording is eligible for purge iff:

1. `terminal_sessions.closed_at IS NOT NULL`, AND
2. `closed_at + retention_days <= NOW()` (inclusive at the
   boundary — a session closed exactly `retention_days` ago is
   eligible), AND
3. At least one row exists in `terminal_recording_chunks` OR
   `terminal_recording_markers` for the session.

Predicate (3) is the natural idempotency keystone: an
already-purged session falls out of the eligible set without any
application-side bookkeeping. A session that was never recorded
also falls out — there is nothing to purge.

Sessions whose `closed_at IS NULL` (status `starting` / `active` /
`detached`) are NEVER purged. The retention clock starts only
once `closed_at` is stamped — by the wired close path
(`POST /api/v1/terminal-sessions/:id/close`), by the future
TTL-expiry close, or by Section 9.3 startup reconciliation
backfilling `closed_at = reconciled_at`. An indefinitely-detached
live PTY does NOT bypass retention; once it eventually closes,
its retention clock starts from that close.

The config field `terminal_recording.max_bytes_per_session`
is a **write-time cap** enforced by the chunk writer (Section 6.1) —
it is NOT a cleanup trigger and the retention worker does NOT
re-evaluate it. A session that hit the byte cap mid-life carries
a `replay_gap { reason: "byte_cap_reached" }` marker and continues
running; its chunks remain readable until normal retention purges
them. A future "storage quota sweep" that triggers cleanup off
total bytes per user / per host is explicitly out of scope for
step 8 and is not designed here.

### 12.3 What is preserved vs deleted

When a session is purged:

| Table                          | Action  | Why                                                                   |
|--------------------------------|---------|-----------------------------------------------------------------------|
| `terminal_recording_chunks`    | DELETE  | The recording corpus is what retention exists to bound.               |
| `terminal_recording_markers`   | DELETE  | Markers are part of the recording table family (Section 5.5). They are NOT a substitute for `audit_events` and MUST NOT outlive the chunks they describe — see the marker-vs-audit decision in 12.5 below. |
| `terminal_sessions`            | KEEP    | Per `SPEC.md` → "Inventory lifecycle and destructive-action policy", `terminal_sessions` are NEVER deleted from any user or system surface. The historical metadata row survives. |
| `session_events`               | KEEP    | Append-only forensic log of session lifecycle; per `SPEC.md` "session_events and audit_events are never deleted". |
| `audit_events`                 | KEEP    | Append-only forensic log; the cleanup itself APPENDS one row, never deletes. |
| `terminal_session_attachments` | KEEP    | Owner-scoped attachment metadata; not part of the recording corpus.   |
| `users`, `hosts`, `ssh_identities`, `server_profiles`, `known_host_entries` | KEEP    | Untouched. Retention is scoped to the recording table family. |

The session row's `closed_at`, `last_seen_at`, `status`, and
`session_events` chain remain readable after purge. A future
"session detail" surface can render "Recording purged on
{purged_at}" by joining `audit_events WHERE kind =
'recording_purged' AND payload->>'target_id' = <session_id>` — but
that join is not part of step 8 itself (12.8 expands).

The cleanup worker MUST NOT:

- delete a `terminal_sessions` row;
- delete or update a `session_events` row;
- delete or update a row in `users`, `hosts`, `ssh_identities`,
  `server_profiles`, `known_host_entries`, or
  `terminal_session_attachments`;
- accept caller-supplied chunk-id or marker-id lists from any
  HTTP surface (the worker is fully system-driven; there is no
  user-triggered purge in v1, see 12.9).

### 12.4 Delete order and transaction shape

The purge of a single session is **one Postgres transaction**:

```
BEGIN;
  -- 1. Aggregate counts and bytes (no payload SELECT).
  --    chunk_count  = count(*) on chunks
  --    marker_count = count(*) on markers
  --    bytes_purged = COALESCE(SUM(byte_len), 0) on chunks
  -- 2. DELETE FROM terminal_recording_markers
  --      WHERE terminal_session_id = $1;
  -- 3. DELETE FROM terminal_recording_chunks
  --      WHERE terminal_session_id = $1;
  -- 4. INSERT INTO audit_events (...) VALUES (..., 'recording_purged', ...);
COMMIT;
```

There is no FK between chunks and markers, so either delete order
is correct. **Recommended order: markers first, then chunks** —
markers are smaller and fewer, and clearing them first leaves a
predictable mid-transaction state if a later step fails. The
order is documented for readability, not correctness.

The aggregates in step 1 read `byte_len` only — never `payload`.
`SUM(byte_len)` and `COUNT(*)` are computed by Postgres without
loading any chunk's bytes into the application process, the wire,
or the query planner's working set in a way the application can
observe. The repository surface that exposes this aggregate MUST
type the response as primitive integers — never `Vec<...Chunk>`
— so a caller cannot accidentally widen the read to `payload`
material.

The audit insert (step 4) sits **inside** the same transaction as
the deletes. Audit failure ROLLBACK reverts the deletes — the
purge is fail-closed. This is a deliberate departure from the
two-phase fail-closed pattern used by the server-profile lifecycle
audit (where the lifecycle row commits before the audit insert
runs and a partial-success orphan is operator-actionable). The
recording purge is irreversibly destructive, so transactional
atomicity is the right shape: either both writes land and an
operator can prove the purge happened, or neither does and the
next sweep will retry.

If a future slice ever needs to relax this (very large purge
batches that don't fit in one transaction, advisory-lock
contention against an unrelated writer), the relaxation MUST
preserve the "every deleted session has a paired
`recording_purged` audit row" invariant. Never break the
audit-pairing rule for performance.

### 12.5 Audit-event behaviour

Cleanup writes one `audit_events` row per session purged.

- **Kind**: `recording_purged` (NEW; requires the audit-kind
  extension migration documented in Section 13 step 8 below). The
  variant lands on `relayterm_core::audit_event::AuditEventKind`
  in lockstep with the migration; a unit test pins the wire tag
  to `"recording_purged"` (matches the existing
  `password_changed` / `session_revoked` pattern).
- **`actor_id`**: `NULL`. The cleanup worker is the system, not
  a user. The existing `audit_events.actor_id` column is
  `REFERENCES users(id) ON DELETE SET NULL` and nullable; the
  pattern matches pre-auth audit rows (`login_failed` for
  unknown emails, `host_key_mismatch` from the preflight probe).
  See 12.9 for what this means for the user-facing audit feed.
- **Payload** (public metadata only — built field-by-field from
  primitives, never `serde_json::to_value` of a domain struct):

  ```json
  {
    "target_id": "<terminal_session_id>",
    "target_kind": "terminal_session",
    "chunk_count": 0,
    "marker_count": 0,
    "bytes_purged": 0,
    "retention_days": 30,
    "closed_at": "2026-04-03T12:00:00Z",
    "purged_at": "2026-05-03T12:00:00Z",
    "reason": "retention_expired"
  }
  ```

  Fields:
  - `target_id` / `target_kind` — match the existing
    audit-payload contract from `SPEC.md` →
    "Audit-event expectations" rule 1.
  - `chunk_count`, `marker_count` — `COUNT(*)` aggregates
    captured before the deletes.
  - `bytes_purged` — `SUM(byte_len)` on chunks (markers are
    metadata-only and contribute 0 bytes by construction).
  - `retention_days` — the active retention policy at sweep
    time. Records the policy in effect so a later operator
    audit can correlate "this purge happened under the old 30d
    policy."
  - `closed_at` — the session's `closed_at` (the field the
    eligibility predicate measured against). Lets an operator
    confirm the threshold without re-querying the (preserved)
    session row.
  - `purged_at` — UTC timestamp at the COMMIT boundary,
    captured by the worker before the INSERT. (`audit_events`
    already has its own `created_at` column with `DEFAULT NOW()`;
    `purged_at` in the payload is the worker's authoritative
    timestamp and matches `created_at` to the millisecond on a
    healthy clock.)
  - `reason` — for v1 always `"retention_expired"`. Reserved
    for future reasons: `"manual_purge"` (operator-triggered),
    `"storage_quota"` (future quota sweep). Step 8 ships only
    `retention_expired`.

- **What MUST NOT appear in the payload**:
  - chunk `payload` bytes, any base64 form of payload, any
    decoded form;
  - marker `payload` JSON contents;
  - `client_info` from any attachment row;
  - hostnames, peer banners, russh / DB error text;
  - `private_key`, `encrypted_private_key`, vault internals,
    session token bytes, token hashes, password hashes,
    bootstrap tokens — the full
    `AUDIT_FORBIDDEN_SUBSTRINGS` set continues to apply;
  - per-chunk seq ranges, per-chunk byte counts, per-chunk ids,
    per-marker kinds, per-marker seqs, or any other field that
    would let an operator dump partial recording shape from
    audit alone. Aggregate counts and total bytes only.

- **Why audit instead of a `purged` recording marker**: a marker
  in `terminal_recording_markers` would (a) require keeping a
  marker row alive past the chunk deletes, breaking the
  "markers are part of the recording corpus and purged with it"
  invariant, AND (b) make `has_recording = false` impossible to
  define cleanly post-purge (a session with one `purged` marker
  has `marker_count = 1`). The right durable home for the
  purge record is `audit_events`, where it sits beside every
  other forensic-grade lifecycle write.

- **Audit-failure policy**: fail-closed at the transaction
  boundary (12.4). If the audit insert fails, the worker logs a
  static category tag (`"audit_insert_failed"`), does NOT
  surface the error text, and moves on to the next eligible
  session in the batch. The session's recording is preserved
  by the ROLLBACK and the next sweep will retry. There is no
  retry-loop inside the worker — bounded batches and the
  next-sweep-retries shape are sufficient (12.7).

### 12.6 Configuration

The retention slice introduces a new `[terminal_recording.cleanup]`
TOML section, alongside the existing `[terminal_recording]`
top-level fields. Existing fields stay where they are; the new
fields are namespaced so they don't collide with the writer's
config knobs.

| Key                                                          | Default          | Bounds            | Notes                                                                            |
|--------------------------------------------------------------|------------------|-------------------|----------------------------------------------------------------------------------|
| `terminal_recording.retention_days` (existing)               | `30`             | `1..=3650`        | Already accepted at boot. Cleanup reads this. Bumping this trims past purges     |
|                                                              |                  |                   | only at the next sweep.                                                          |
| `terminal_recording.cleanup.enabled` (NEW)                   | `true`           | bool              | Independent of `terminal_recording.enabled` — see below.                         |
| `terminal_recording.cleanup.startup_sweep_enabled` (NEW)     | `true`           | bool              | Run a single sweep at boot AFTER reconciliation, BEFORE the listener binds.      |
| `terminal_recording.cleanup.sweep_interval_seconds` (NEW)    | `21600` (6h)     | `0` OR `60..=604800` | Periodic sweep cadence. The sentinel `0` disables the periodic worker entirely  |
|                                                              |                  |                   | (Stage B); the startup sweep still runs if `startup_sweep_enabled = true`. Any   |
|                                                              |                  |                   | non-zero value MUST be in `60..=604800` — sub-60s cadence creates a              |
|                                                              |                  |                   | thundering-herd against an empty corpus, and `> 7d` defers retention past the    |
|                                                              |                  |                   | default `retention_days = 30` window without operator intent. The validator      |
|                                                              |                  |                   | rejects any non-zero value below 60 with a typed error.                          |
| `terminal_recording.cleanup.batch_size` (NEW)                | `100`            | `1..=10000`       | Max sessions purged per sweep iteration. Each session is its own transaction.    |

Environment-variable overrides follow the existing convention
(`RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED`,
`RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS`,
etc.).

**Independence from `terminal_recording.enabled`** — load-bearing.
The cleanup worker MUST run even when
`terminal_recording.enabled = false`, as long as
`cleanup.enabled = true`. Reasoning: an operator who turns
recording off after running it for some time MUST NOT have their
existing recording corpus become immortal — that would be the
opposite of the privacy posture. The recording writer is gated
on `terminal_recording.enabled`; the cleanup worker is gated on
`cleanup.enabled`. The two switches are independent and serve
different purposes.

The matching boot-validation rule:
- `cleanup.enabled = true` is permitted regardless of
  `terminal_recording.enabled`. No master-key check is required
  for cleanup — purge does not read chunk `payload` bytes
  (12.10), only `byte_len` aggregates and ids, so it does not
  need to decrypt anything.
- `cleanup.enabled = false` is permitted in dev for contributors
  exercising the writer in isolation. In production it is a hard
  warn-at-boot ("cleanup disabled — recording corpus will grow
  unbounded") but NOT a boot failure: an operator may legitimately
  want to manage retention out-of-band (DB-side cron, external
  pipeline, vacuum tooling). The warn-at-boot is the operator-
  visible reminder.

### 12.7 Worker timing and lifecycle

The retention work is staged across two implementation slices.
Neither slice runs unless `cleanup.enabled = true`.

**Stage A (step 8a) — startup-only sweep.** A single sweep runs
at boot with the same boot-ordering rule as Section 9.3
reconciliation: AFTER the database pool is ready, AFTER startup
reconciliation, BEFORE the HTTP listener binds, BEFORE the
`TerminalSessionManager` is constructed. Bounded to one batch
(`batch_size`) so a long retention backlog does not block boot
indefinitely; remaining work is picked up by Stage B's first
periodic tick. If `startup_sweep_enabled = false` the boot skips
the sweep entirely.

**Stage B (step 8b) — periodic managed worker.** A managed
background task spawned from `AppState` after the listener binds.
The task owns:

- a `tokio::sync::watch` (or equivalent) shutdown channel wired
  to the same graceful-shutdown signal the listener uses;
- a `tokio::time::interval` driven by
  `cleanup.sweep_interval_seconds`;
- on each tick, scan up to `batch_size` eligible sessions and
  purge them (each in its own transaction per 12.4).

The task is NEVER `tokio::spawn`-and-forget. It returns a
`JoinHandle` (or rides on `JoinSet`) so shutdown can `await` its
completion. Mandated by the no-spawn-and-forget rule in
`AGENTS.md` "Encountered Lessons" 2026-05-02 (originally written
for the `last_seen_at` touch in the auth path; the rule
generalises to every managed background task).

**Concurrency safety.** Two periodic ticks must not run
concurrently against the same sweep. The single-task design
already guarantees this for a single-process deployment. For a
future multi-instance deployment, the worker takes a Postgres
advisory lock (e.g. `pg_try_advisory_lock(<fixed_id>)`) at the
start of each tick and releases it at the end; a second instance
that fails to acquire the lock skips the tick (no error, no
log spam) — only one node sweeps at a time. Step 8 ships the
advisory-lock dance even if the deployment is single-instance,
because the cost is one round-trip and the safety is global.

**Failure semantics.**

- Stage A startup sweep failure (DB error, advisory-lock
  contention, audit-insert rollback) is **not** fail-fast. The
  boot proceeds; a `warn!` line names the static category tag
  only (`"retention_sweep_failed"`); operators see "boot
  succeeded but retention deferred". Rationale: missing one
  sweep cycle is operationally undesirable but is not a
  security-relevant correctness issue (orphaned recording rows
  are not a security risk per se — the data was already
  authorised to exist, retention just trims it). This is a
  deliberate departure from Section 9.3 reconciliation, which
  IS fail-fast — reconciliation correctness affects the
  user-facing session list and the live PTY recovery path,
  while retention correctness affects only durable corpus size.
- Stage B periodic-tick failure logs the same static category
  tag and continues. The next tick retries.
- Per-session purge failure (covered by the transaction
  rollback in 12.4) increments a static error counter but does
  not abort the rest of the batch — other sessions in the same
  tick proceed normally.

**Logging surface.** Every operator-side log line in the worker
is a static category tag plus public ids and primitive counts
only — never `?err` formatting that could round-trip driver
text, never the chunk payload in any form, never marker payload
JSON. The worker's `Debug` impl exposes the variant tag and (when
enabled) the static configuration; it never includes any session
data. Same posture as the chunk writer's `Debug` rule
(Section 6.1).

### 12.8 API / UI behaviour after purge

A purged session looks identical to a never-recorded session on
the read API surface (Section 10):

- `GET /api/v1/terminal-sessions/:id/recording/metadata`:
  ```json
  {
    "terminal_session_id": "<uuid>",
    "has_recording": false,
    "chunk_count": 0,
    "marker_count": 0,
    "first_seq": null,
    "last_seq": null,
    "first_recorded_at": null,
    "last_recorded_at": null
  }
  ```
  This is the exact shape the metadata route already returns
  today for a session that was never recorded (Section 10.1).
  No new field is added in step 8; "purged" and "never
  recorded" collapse to the same wire shape. Distinguishing
  them requires a join through `audit_events` (12.5) which the
  metadata route deliberately does NOT do.
- `GET /api/v1/terminal-sessions/:id/recording/chunks` —
  returns `200 []`.
- `GET /api/v1/terminal-sessions/:id/recording/markers` —
  returns `200 []`.

**Replay viewer behaviour after purge** is unchanged from the
existing "no recording available" path (Section 11). The viewer
reads the metadata endpoint, sees `has_recording = false`, and
renders the existing honest-copy empty state. There is no
purge-specific banner in step 8.

A purpose-built "Recording was purged on {date} (retained for
{retention_days} days from session close)" banner is **future
work**, not in scope for step 8. When it lands it consumes the
audit row's `purged_at` / `retention_days` / `closed_at` payload
fields, joined through `audit_events.kind = 'recording_purged'
AND payload->>'target_id' = <session_id>`. Until that slice
ships, the audit-events read API
(`/api/v1/audit-events/recent`) is the operator-facing
"my recordings just got swept" signal — see 12.9 for the
visibility caveat.

The session list, dashboard summary, and recent-activity
surfaces continue to NOT join through the recording tables
(Section 10.5); a purge changes nothing about what those
surfaces render.

**Frontend cache discipline (load-bearing).** A future replay
viewer that ever caches recording metadata must invalidate on
`metadata.has_recording === false` — never paint stale chunks
from a previous fetch into a viewer whose backing store has
since been purged. For step 5's existing viewer this is a no-op
(it already fetches metadata once per mount and never caches
across mounts); pinning the rule here so a future caching
optimisation does not regress it.

### 12.9 Owner / admin visibility

- The cleanup worker is **system-wide and owner-agnostic**.
  Eligibility is driven by `closed_at + retention_days`, never
  by an `owner_id` filter. A user's recordings are purged on
  the same schedule as everyone else's.
- User read access stays owner-scoped through
  `AuthenticatedUser` and `terminal_sessions.owner_id ==
  user.user_id()` (Section 10). A user querying a foreign
  session's metadata / chunks / markers continues to receive
  a byte-identical 404 — pre-purge AND post-purge.
- **No user-triggered purge in v1.** There is no `POST
  /api/v1/terminal-sessions/:id/recording/purge` route, no
  user-facing "delete recording" affordance, no bulk-purge
  affordance. Adding one introduces a destructive surface
  that needs CSRF, confirmation copy, and its own audit kind
  (`recording_user_purged` or similar) — out of scope for
  step 8.
- **No admin / cross-user purge UI in v1.** RelayTerm has no
  admin/RBAC story today (`SPEC.md` "no admin / cross-user
  audit view" rule); cross-user retention overrides arrive
  with that broader admin slice, not with step 8.
- **`recording_purged` is invisible to the user-facing audit
  feed by construction.** The current
  `GET /api/v1/audit-events/recent` route filters with
  `WHERE actor_id = $caller` (per the
  `recent_for_actor` rule documented in `AGENTS.md`
  "Encountered Lessons" 2026-05-01). `recording_purged` rows
  carry `actor_id = NULL` (system actor), so they are
  excluded — the user-facing feed never grows to include
  retention bookkeeping. This is **intentional** for v1: a
  multi-user deployment's recent-activity panel cleanly
  separates "things this user did" from "things the system
  did to this user's data."

  A future per-user "system actions affecting your data"
  surface MUST NOT relax the `actor_id = $caller` filter on
  `recent_for_actor` — that would expose every user's
  retention sweep to every other user via NULL-actor leak. The
  correct shape is a separate route that joins
  `audit_events` (system kinds) on `terminal_sessions.owner_id
  = $caller`. Step 8 leaves that join unimplemented.

### 12.10 Safety, redaction, and concurrency

The cleanup worker inherits every redaction rule the rest of the
recording subsystem already enforces. The load-bearing items
specific to retention:

- **Never SELECT `payload` bytes.** The worker reads
  `byte_len`, `id`, and `terminal_session_id` only. The
  aggregate query (12.4 step 1) is `SELECT COUNT(*), COALESCE
  (SUM(byte_len), 0) FROM terminal_recording_chunks WHERE
  terminal_session_id = $1`; the eligibility query is on
  `terminal_sessions` columns plus `EXISTS (SELECT 1 FROM
  terminal_recording_chunks WHERE ...)`. Neither query
  references `payload`, neither column projection pulls it.
  A repository-test pins this with a sentinel byte string in a
  fixture chunk and asserts the byte string never appears in
  any returned domain object, formatted error, or `tracing::*`
  line emitted by the worker.
- **Audit payload public-only.** Mirror the existing
  `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel test pattern from
  `crates/relayterm-api/tests/api.rs`: drive a synthetic PTY
  workload through the writer (containing `private_key`,
  `BEGIN OPENSSH PRIVATE KEY`, `password=`, and a unique
  random fixture string), close the session, advance the
  fixture clock past `retention_days`, run the worker, and
  assert NONE of those sentinels appears in the
  `recording_purged` row's `payload`, the worker's
  `tracing::*` lines, or any 5xx error body.
- **Deletion by `terminal_session_id` and eligibility only.**
  The repository surface is `delete_recording_for_session
  (TerminalSessionId)` (or similar) — never `delete_chunks
  (Vec<ChunkId>)`. There is no HTTP route that takes
  caller-supplied chunk-id lists. A future user-purge route
  (12.9, deliberately out of scope) would still take a
  session id and re-derive the chunks server-side, never a
  client-supplied id list.
- **Owner-scope is irrelevant at the worker layer because
  there is no caller.** The worker is system-driven; it does
  NOT consult `AuthenticatedUser`, does NOT take a `UserId`,
  does NOT scope by `owner_id`. The eligibility predicate
  applies uniformly.
- **Concurrency and idempotency.** The advisory-lock dance
  (12.7) ensures one sweep at a time across a multi-instance
  deployment. Within a single sweep, batched per-session
  transactions (12.4) ensure that a partial failure does not
  cross-contaminate other sessions. Idempotency is a schema
  invariant: once a session's chunks and markers are deleted,
  the eligibility predicate (12.2 step 3) excludes it from the
  next sweep — re-running the worker is a byte-identical
  no-op.
- **Logging discipline.** Static category tags only; no
  `?err`-formatted driver text, no chunk bytes, no marker JSON.
  See 12.7's logging-surface paragraph.
- **No background third-party processing.** The cleanup worker
  does NOT stream purge events to an external service, search
  indexer, or notification surface. The DB is the only sink
  (the audit row IS the sink).

### 12.11 Implementation order

This is the staged rollout for the retention work. Each step is
its own slice; nothing ships unless the prior step is green.
This list refines Section 13 step 8 of the broader recording
plan.

1. **This design slice** (current). Doc only. No code, no
   migrations, no runtime behaviour change.
2. **Audit-kind extension + repository purge method**
   (step 8a-prep). Migration extends the
   `audit_events_kind_chk` CHECK with `recording_purged`;
   `AuditEventKind` Rust enum gains the variant; serde tag
   pinned by unit test; `.sqlx/` regenerated. Repository
   gains `delete_recording_for_session(TerminalSessionId) ->
   Result<DeleteSummary>` returning `{ chunk_count,
   marker_count, bytes_purged }`. No worker, no caller, no
   route.
3. **Cleanup config block** (step 8a-prep). Add
   `[terminal_recording.cleanup]` to `apps/backend/src/config.rs`
   with the bounds in 12.6. Production-validation envelope
   warns on `cleanup.enabled = false`. No worker yet.
4. **Startup-only sweep** (step 8a). Wire the worker into
   `apps/backend/src/main.rs` AFTER reconciliation, BEFORE the
   listener binds. Bounded to one batch. Failure is `warn!` +
   continue (12.7). Audit row written per session purged.
5. **Periodic managed worker** (step 8b). Spawn the managed
   background task on `AppState`. Advisory-lock dance per
   tick. Graceful-shutdown wired to the same signal future
   the listener uses.
6. **UI copy for purged recordings** (later, optional). Replay
   viewer banner that distinguishes "purged" from "never
   recorded" by joining `audit_events`. Only when an operator
   asks for it; not part of step 8 itself.
7. **Operator retention metrics / dashboard** (later,
   optional). Counters of swept sessions per cycle, total
   bytes reclaimed, last successful sweep time. Belongs in a
   future operator-metrics slice; not part of step 8 itself.

### 12.12 Tests required for the implementation slices

The implementation slices above MUST add the following classes
of test. None are written in this design slice; they are the
contract a future code-reviewer enforces.

- **Eligibility — closed session past threshold is purged.**
  Fixture: a closed session with `closed_at = NOW() -
  (retention_days + 1 day)`, plus chunk + marker rows. After
  one worker run: zero chunks, zero markers, one
  `recording_purged` audit row. The session row is preserved.
- **Eligibility — closed session at exact threshold is purged.**
  `closed_at = NOW() - retention_days` exactly. Inclusive
  boundary semantics (12.2). A pinned test prevents the
  off-by-one regression.
- **Eligibility — closed session before threshold is preserved.**
  `closed_at = NOW() - (retention_days - 1 day)`. Zero deletes,
  zero audit rows.
- **Active / detached / starting sessions are NEVER purged.**
  Three fixtures with `closed_at IS NULL` and
  `status IN ('active', 'detached', 'starting')` — each with
  chunk rows. After the worker runs: chunk rows untouched, no
  audit row. (A live PTY whose chunks are 100 days old does
  NOT get its chunks swept; the clock starts at close.)
- **Already-purged session is a no-op.** A session with
  `closed_at` past threshold but zero chunks AND zero markers
  (already purged earlier). After the worker runs: no audit
  row written, no transaction begun against it.
- **`terminal_sessions` row preserved after purge.** Pre / post
  row equality except for nothing — the row is byte-identical
  before and after.
- **`session_events` rows preserved after purge.** All
  pre-existing `session_events` rows for the session continue
  to exist post-purge — same byte-identical equality check as
  the `terminal_sessions` row test above (kind, payload, seq,
  created_at compared row-for-row pre vs post). Pinned
  separately from the `terminal_sessions` test for parity with
  the "audit_events KEEP" rule (the `recording_purged` row
  IS appended; pre-existing audit rows for the session must
  not be touched).
- **Audit row: kind, actor, payload shape.** The inserted row
  has `kind = 'recording_purged'`, `actor_id = NULL`, and
  `payload` keys exactly `{ target_id, target_kind,
  chunk_count, marker_count, bytes_purged, retention_days,
  closed_at, purged_at, reason }`. `target_kind ==
  'terminal_session'`, `reason == 'retention_expired'`.
- **Audit row: redaction sentinels.** Drive synthetic PTY
  bytes containing `BEGIN OPENSSH PRIVATE KEY`, a unique
  random sentinel, `password=hunter2`, and `data_b64=...`
  through the writer pre-purge. After purge: NONE of those
  sentinels appears in any `recording_purged` row, in any
  `tracing::*` line emitted by the worker, in any 5xx error
  body, or in any `Debug` output of the worker / repository
  delete summary.
- **Audit-failure rolls back deletes.** Force the audit insert
  to fail (e.g. constraint violation via a faulty test seam).
  After: chunks and markers are STILL present; no
  `recording_purged` row exists; the static error category
  tag was logged; the session remains eligible for the next
  sweep.
- **No `payload` SELECT.** A repository-test asserts that the
  aggregate query and the eligibility query do NOT load the
  chunk `payload` column. Implementation seam: the repository
  exposes the aggregate as `(chunk_count: i64, marker_count:
  i64, bytes_purged: i64)`, NOT as `Vec<TerminalRecordingChunk>`.
  A unit test against an in-memory fake AND a Postgres
  integration test pins the projection.
- **Batch size is honoured.** With 5 eligible sessions and
  `batch_size = 2`, one worker tick purges exactly 2; the
  next tick purges another 2; the third purges the last 1.
- **Idempotency on a second sweep.** After a clean sweep,
  re-run the worker immediately. Zero deletes, zero audit
  rows.
- **Cleanup runs even when `terminal_recording.enabled =
  false`.** Fixture: pre-existing chunks and markers from a
  previous deploy. Boot with `terminal_recording.enabled =
  false` AND `cleanup.enabled = true`. Eligible rows are
  swept normally.
- **`cleanup.enabled = false` skips the work.** Boot with the
  flag off; the worker is not constructed; eligible rows are
  preserved.
- **Owner-scope unaffected by purge.** A user GET against a
  foreign session id continues to return a byte-identical
  404 both pre-purge and post-purge.
- **`recording_purged` does NOT appear in the user-facing
  audit feed.** A test against `GET /api/v1/audit-events/recent`
  after a purge: the response array does NOT include any
  `recording_purged` row, regardless of whose session was
  swept. The NULL-actor exclusion is the load-bearing rule
  (12.9).
- **Concurrency: advisory lock prevents double-sweep.** Two
  test workers racing the same eligibility set; one acquires
  the advisory lock, the other skips silently. Total purges
  across both = exactly one per session.
- **Graceful shutdown.** A periodic worker mid-tick when the
  shutdown signal fires completes the in-flight per-session
  transaction (or rolls it back cleanly if not yet
  committed) and exits within a bounded deadline. No
  detached `tokio::spawn` futures.
- **Recording-disabled means no chunks (existing test, named
  here for completeness).** The Section 14 test "Recording
  disabled means no chunks" continues to pass; cleanup is
  orthogonal.

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
3. **Orchestrator writes chunks + markers** (writer foundation
   landed; `attached`/`detached`/`reattached` markers deferred).
   - **Landed (this slice)**:
     - `RecordingWriter` + `RecordingRuntime` in
       `crates/relayterm-terminal/src/recording.rs`. The writer is
       a tee from the PTY forwarder, never a gate — the live wire
       cannot stall on it.
     - Manager wiring: `TerminalSessionManager::with_recording`
       attaches a runtime; `start_live_pty` spawns one writer task
       per session; the forwarder calls `recording.record_output`
       AFTER fanning each frame to broadcast + replay.
     - Chunk batcher: accumulates consecutive output frames up to
       `chunk_target_bytes`, never overshoots `chunk_hard_cap_bytes`,
       flushes the trailing partial chunk on shutdown.
     - Bounded queue (256 commands) between forwarder and writer
       task. On overflow the producer drops the new frame and
       extends a pending gap; on next successful enqueue the writer
       flushes its open chunk and emits a `replay_gap` marker
       carrying `{ from_seq, to_seq, reason: "writer_overflow" }`.
       Same path covers DB-write failure (`reason: "writer_error"`)
       and oversized single frames (`reason: "frame_oversized"`).
     - Markers: `started` at `seq = 0` on writer construction,
       `closed` at the highest observed seq on shutdown, `resized`
       on every successful `resize_session` against a live PTY.
     - Marker payloads are public-safe metadata only (counts,
       dims, reason codes) — built field-by-field from explicit
       primitives.
     - Shutdown is bounded (5s deadline) and the manager awaits
       the forwarder before signalling shutdown so trailing
       frames are observed before the closed marker lands.
     - Supported runtime mode: only `terminal_recording.enabled =
       true` AND `encryption.mode = disabled` (plaintext-at-rest).
       `enabled = true` AND `encryption.mode = required` is
       config-valid but the writer for that mode has not landed;
       the backend now FAILS TO BOOT in that combination rather
       than silently degrade to plaintext.
     - Errors are logged operator-side as static category tags
       only — never the bytes, never `?err` formatting that could
       round-trip driver text. The writer's `Debug` impl exposes
       only the variant tag and (when enabled) the session id.
     - Tests landed: writer unit tests in
       `crates/relayterm-terminal/src/recording.rs` (disabled
       no-op, started+closed lifecycle, batching, target-flush,
       hard-cap split, oversized-frame gap, writer-error gap,
       seq-zero rejection for non-started, payload-order
       preservation, byte-sentinel redaction); manager
       integration tests in
       `crates/relayterm-terminal/tests/recording_writer.rs`
       (recording-disabled writes nothing, recording-enabled
       writes chunk + started + closed, resize emits resized
       marker at last seq, sentinel never appears in any marker
       payload / Debug, input bytes are never recorded).
   - **Deferred to a follow-up slice**:
     - `attached` / `detached` / `reattached` markers — these
       require threading the live writer handle from the
       attach/detach path through the manager, and the slice's
       scope was capped at the always-on lifecycle markers
       (started/closed) plus resize. The schema CHECK already
       allows the variants; only the emit-site is missing.
     - Chunk continuity assertion test against a real Postgres
       row (the in-memory fake covers the invariant). The
       repository layer's existing per-row CHECKs and the unique
       `(session_id, seq_start)` index are the durable backstop.
     - Encryption-aware writer (the `encryption.mode = required`
       path; landing this unblocks production recording).
4. **Durable replay read API** (foundation landed).
   - **Status**: the three HTTP endpoints in Section 10.1 / 10.2 /
     10.3 have landed in
     `crates/relayterm-api/src/routes/v1/terminal_recordings.rs`,
     backed by `TerminalRecordingRepository::get_metadata`,
     `list_chunks`, `list_markers` (the metadata method is the new
     trait surface added in this slice). All three routes resolve
     the addressed `terminal_session` through `AuthenticatedUser` +
     the existing `terminal_sessions.owner_id == user.user_id()`
     filter; foreign and missing sessions collapse to a
     byte-identical 404. Reads write zero `audit_events` rows.
   - **Behaviour caveat versus the original sketch**: an empty
     recording (no chunks AND no markers) returns `200` with
     `has_recording = false` rather than `404`, so a future UI can
     distinguish "session exists, never recorded" from "session
     does not exist." The Section 10.1 sketch said `404`; the
     implementation chose `200 + has_recording: false`.
   - Owner-scope tests
     (`recording_routes_foreign_owned_returns_indistinguishable_404`),
     auth tests
     (`recording_read_routes_return_401_without_session_cookie`),
     bounded `limit` clamp tests, negative-`from_seq` 400 tests,
     and base64 round-trip + sentinel-not-in-JSON redaction tests
     all pin the contract.
   - Frontend replay viewer (step 5) — foundation has now landed
     (see step 5 below).
   - Encryption-aware decode is still ahead. The writer only emits
     `encryption = 'none'` rows, so the route returns the
     post-persistence bytes verbatim as base64. When the
     `recording_v1` envelope lands the route MUST decrypt
     server-side; the wire MUST NOT carry envelope ciphertext.
5. **Frontend replay viewer for closed sessions** (foundation landed).
   - **Status**: read-only viewer at
     `apps/web/src/lib/app/views/RecordingReplayView.svelte`,
     reachable from the Sessions list via a new `View recording`
     action. The shell holds the replay session id in transient
     in-memory state (`activeReplaySessionId`, NOT a sidebar nav
     entry, NOT mirrored into the URL — recording chunk bytes are
     sensitive; we keep them off any externally observable
     surface) and renders the viewer ABOVE the navigation switch
     so any nav click clears it.
   - The viewer uses a fresh `XtermRenderer` constructed with
     `xtermOnly: { disableStdin: true }` and does NOT subscribe
     to `onInput` — input is disabled at the renderer AND no
     listener exists to forward it. There is no live
     `TerminalSessionClient`, no `WebSocketTerminalTransport`,
     and no wire `attach` handshake.
   - The Sessions list deliberately does NOT pre-fetch metadata
     for every row (would be N+1 against
     `/recording/metadata`). The affordance is offered for
     `detached` and `closed` rows only — `active` rows route to
     the live `Open` action, `starting` rows have nothing to
     replay; the viewer's metadata gate honestly surfaces "No
     recording available" when an opened session turns out empty.
   - Decoded chunk bytes go straight to `renderer.write(...)`
     and are never stashed in `$state`, never persisted to
     `localStorage` / `sessionStorage`, never logged. The
     viewer renders markers as a metadata strip only
     (`seq`, kind, `created_at`, a truncated JSON preview of the
     opaque `payload`).
   - Banner copy is honest: replay-only, output-only, input was
     not recorded, the live SSH session cannot be resumed from a
     recording, backend-restart recovery is not implemented yet.
   - Helpers and tests:
     `apps/web/src/lib/api/terminalRecordings.ts` adds typed
     `getTerminalRecordingMetadata` / `Chunks` / `Markers`
     helpers (every request uses `credentials: "include"`,
     every session id is path-encoded), strict
     `parseRecording*` parsers that drop unknown sibling fields
     by construction, an `isSupportedChunk` guard that requires
     `encryption == "none"` AND `compression == "none"`, and a
     `decodeRecordingChunk` helper that decodes base64 once and
     re-validates against the chunk's declared `byte_len`.
     `describeRecordingError` and `describeDecodeFailure` stay
     functions of the discriminant only — never echo the wire
     `message` field of an HTTP error, the thrown `Error.message`
     of a transport failure, `data_b64`, raw chunk bytes, or any
     vault / auth sentinel. Sentinel tests in
     `apps/web/tests/terminalRecordingsApi.test.ts` pin the
     redaction posture against `data_b64`, the decoded chunk
     payload, `private_key`, `encrypted_private_key`,
     `session_token`, `token_hash`, `password_hash`, and
     `first_user_bootstrap_token` — none reach a parsed DTO,
     formatted error string, or a localStorage / sessionStorage
     write.
   - **Deferred to a follow-up slice**: live attach fallback to
     durable chunks (the in-memory ring is still authoritative
     on a live attach), retention cleanup worker,
     encryption-aware decode, export / download UI, recording
     search, admin / cross-user replay, production renderer
     selector for the replay surface, and mobile /
     Tauri-specific replay UX. (Startup reconciliation +
     closed-marker reconciliation have landed; see Section
     9.3.) The original sketch
     also called for adding a `replay_only` variant to
     `TerminalSessionState`; this slice intentionally avoids
     that public-API change because the viewer does not use a
     `TerminalSessionClient` at all — there is no client state
     machine to extend, so the breaking change is unnecessary.
     If a future slice merges the live + replay viewers behind
     one client surface, the variant addition becomes the
     correct way to discriminate, and the breaking-change
     protocol from the original sketch applies.
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
8. **Retention cleanup job** (design landed in Section 12; worker not
   yet implemented).
   - Section 12 is the binding contract — eligibility, delete order,
     transactional audit, config, worker timing, post-purge API/UI
     behaviour, owner/admin visibility, redaction, implementation
     order (12.11), and tests (12.12). The bullets below are the
     audit-kind extension protocol the slice MUST follow on top of
     Section 12; both must be satisfied.
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
  "startup_reconciliation"` and a `previous_status` field, (b) a
  `closed` recording marker at the highest persisted seq with
  `payload.reason = "startup_reconciliation"`, and (c)
  idempotency on a second restart (no duplicate `session_events`
  row, no duplicate `closed` marker — neither when a marker
  already exists at the same seq nor on a fresh second pass).
  Section 9.3 is the canonical contract.
- **Retention cleanup**. The cleanup worker, run against a
  fixture with sessions older than the retention window, deletes
  exactly the chunk and marker rows for those sessions, leaves
  the `terminal_sessions` rows in place, and writes one
  `recording_purged` audit row per swept session. Section 12.12
  is the full test list (eligibility boundaries, transactional
  audit rollback, batch size, idempotency, owner-scope, NULL-actor
  exclusion from the user-facing audit feed); the bullet here
  is the one-line summary.
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
