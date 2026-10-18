# Compress large server messages before E2EE encryption

Status: **Approved** on 2026-10-13 (with the safeguards below). Tracking issue:
[#89](https://github.com/mubeda/BibCode/issues/89).

## Problem and evidence

A remote client reaches the server over an end-to-end encrypted WebSocket
(Noise records, `apps/server/src/rpc/e2ee.rs`). Over Tailscale the link
delivered 1–2 MB/s. Before thread patches (#89, commit e81837b4) the server
pushed 2.2 GB of whole-thread snapshots in a few hours and every small reply
queued behind them for 7–10 s.

Thread patches remove the per-event snapshots. Large messages remain on the
first read of a thread and on resubscription after a reconnect: one snapshot of
the measured thread (40,170 activities) is 6–10 MB, five to ten seconds of a
1–2 MB/s link, during which the client's other replies wait. Other bulk replies
(Git diffs, pull request files, process diagnostics) behave the same way.

Payloads are JSON with highly repeated keys and values. Ciphertext does not
compress, so the WebSocket's own `permessage-deflate` is useless on an E2EE
connection: compression has to happen before encryption.

## Alternatives

1. **Per-message deflate before encryption, server to client, above a size
   threshold (recommended).** The server compresses an outbound message's JSON
   with raw deflate when it is at least 16 KiB and the peer advertised support;
   the first record's flag marks it. Each message is compressed on its own, with
   no shared dictionary. Client messages stay uncompressed.
2. **Bound the first thread snapshot** (send the newest N activities and page
   older ones on demand). Removes the largest message at its source but changes
   the thread view and its contract; a much larger change. Complementary, not a
   substitute: diffs and file lists stay large.
3. **Do nothing more.** Patches already remove the steady flood; first loads
   and reconnects of long threads still block a slow link for seconds.

## Security trade-off (needs explicit approval)

Compressing before encrypting makes ciphertext length depend on content. An
attacker who can both **inject chosen text into the same message** as a secret
and **observe the encrypted message lengths** on the network can, over many
attempts, test guesses of that secret (the CRIME/BREACH class of attack).

In BiBCode, a thread snapshot can contain text an agent fetched from a hostile
source next to text the user typed or tools printed, and the encrypted lengths
are visible to anyone on the network path (Tailscale peers, the relay, a hotel
network). Mitigations in this design:

- server-to-client only; nothing the user types is compressed on its way out;
- per-message compression, no shared dictionary, so only secrets within the
  same message are exposed, and only through that message's length;
- a 16 KiB threshold, so small interactive replies (keystrokes, Pongs, unary
  answers) keep constant-ratio lengths;
- negotiated: an environment switch can turn it off; peers that do not advertise
  support get today's behaviour.

The residual risk is a length side channel on large server messages that mix
attacker-controlled and secret text. Approve this design only if that is
acceptable for remote use; otherwise choose alternative 2 or 3.

## Design (alternative 1)

- **Negotiation.** The client's `e2ee_auth` lists `deflate-v1` in its existing
  `features`, and the server confirms it in `e2ee_authenticated` or leaves it
  out (revised during implementation: the existing feature list instead of a
  separate `compression` field). Older peers on either side keep uncompressed
  records.
- **Framing.** A new flag bit, `E2EE_RECORD_FLAG_DEFLATE = 0x10`, set on every
  record of a compressed message and combined with the existing final (`0x00`)
  and continuation (`0x01`) values; `0x02` stays the interleaved control
  record. The compressed message is a 4-byte big-endian uncompressed length
  followed by raw deflate data, split into records as today. The record and
  message limits apply to the compressed size; the declared length must not
  exceed the message limit and the output must match it exactly, so a bomb
  fails the connection without unbounded allocation.
- **Server.** `flate2` raw deflate at level 3 on the writer's encode step,
  only for messages of at least 16 KiB. Measured CPU belongs in the Profiler's
  existing writer timing.
- **Client.** `DecompressionStream("deflate-raw")` (Chromium 80+, Safari 16.4+,
  current WebKitGTK); if it is unavailable the client does not advertise
  support.
- **Kill switch.** `BIBCODE_REMOTE_COMPRESSION=0` in the server's environment
  stops the server from confirming the feature (revised during implementation:
  an environment switch instead of a settings toggle).

## Tests

- Round trip: a 1 MiB snapshot compresses, splits into records, reassembles and
  decompresses to the same bytes; a 15 KiB message stays uncompressed.
- Negotiation: an old client gets no compressed records; an old server ignores
  the advertisement.
- Limits: a decompressed size above the message limit fails the connection
  without allocating the whole output.
- Measurement: the Profiler's per-connection outbound bytes before and after on
  the Tailscale client.

## Out of scope

Bounding the first thread snapshot (alternative 2), client-to-server
compression, and shared dictionaries.

## Addendum (2026-10-17): compression on plain `/ws`

Status: **Approved** on 2026-10-17 by the user's request to compress large
messages on plain `/ws`.

### Problem

SSH environments and plain remote servers use plain `/ws`, not `/ws-e2ee`, so
the design above does not reach them. Relayed VPN links measure 236–252 ms round
trips; the same multi-megabyte first thread snapshot and reconnect snapshots
cross them uncompressed. Plain `/ws` already has record framing:
`bibcode.rpc.chunked.v1` sends messages over 64 KiB as binary records in the
E2EE record format.

### Alternatives

1. **WebSocket `permessage-deflate` (RFC 7692).** The pinned stack cannot do it:
   `axum` 0.8.9's `WebSocketUpgrade` uses `tokio-tungstenite` 0.29, and neither
   `tungstenite` 0.29 nor 0.30 implements the extension (0.30 only parses the
   header in a test), so it would mean replacing the WebSocket library. It
   would also compress every message, keystrokes and acknowledgements included,
   which the size threshold above exists to avoid, and with context takeover
   it keeps a compression window per connection in both peers.
2. **Reuse `deflate-v1` records, negotiated by a second subprotocol
   (chosen).** The client offers `bibcode.rpc.chunked-deflate.v1` before
   `bibcode.rpc.chunked.v1`. The selected subprotocol means chunked records plus
   compressed server messages, exactly as `deflate-v1` defines them on E2EE.
   Negotiation completes in the upgrade itself, with no extra round trip.
3. **An in-band feature message** after the socket opens, like `e2ee_auth`.
   Plain `/ws` has no such exchange; adding one costs a round trip on every
   connect or a protocol message that old servers reject.

### Design (alternative 2)

- **Negotiation.** The server lists its subprotocols in preference order:
  `bibcode.rpc.chunked-deflate.v1`, then `bibcode.rpc.chunked.v1`, and picks the
  first one the client offered. `BIBCODE_REMOTE_COMPRESSION=0` removes the
  deflate one, so the server falls back to chunked records. An old server does
  not know the new name and selects `bibcode.rpc.chunked.v1`; an old client
  never offers it. Both keep today's framing.
- **Client opt-in.** The client offers the deflate subprotocol only when
  `DecompressionStream("deflate-raw")` exists and the socket is not a
  same-machine backend: a loopback socket URL counts as local unless the
  connection is an SSH tunnel, whose local end is loopback by construction.
  Compressing on loopback spends CPU for no saving.
- **Framing.** Unchanged from `deflate-v1`: a data message of at least 16 KiB
  that compresses smaller leaves as a 4-byte big-endian uncompressed length and
  raw deflate data (level 3, on a blocking thread, abandoned when its write is
  dropped), split into records that all carry `0x10`. On plain `/ws` a
  compressed message always travels as binary records, even below the 64 KiB
  record threshold. Text frames stay whole uncompressed messages. Client
  messages and control records are never compressed. Limits are the plain
  record limits: 64 MiB uncompressed and 2,048 records per message.
- **Client.** The plain record socket shares the E2EE socket's inflation and
  its ordered delivery: a declared length above 64 MiB or output that differs
  from it fails the socket, and messages reach the RPC client in arrival order
  while one inflates.

### Security

Plain `/ws` has no end-to-end encryption, so its content is already visible to
anyone on an unencrypted path; on SSH, WireGuard or TLS paths only lengths are,
as on E2EE. The length side channel and its mitigations are the ones above:
server to client only, per message, at least 16 KiB, and the kill switch.

### Tests

- Negotiation: the server selects the deflate subprotocol only when offered and
  enabled; the kill switch and an old client get chunked records.
- Framing: a large server message on a negotiated socket arrives as `0x10`
  records and reassembles to the same text; a 15 KiB one stays a text frame.
- Client: a deflate-negotiated plain socket inflates, preserves order, and
  rejects an oversized declared length; a loopback primary does not offer it,
  an SSH connection does.
- Measurement: bytes received for a large thread snapshot with
  `scripts/measure-remote-latency.ts`, before and after.
