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

- **Negotiation.** The client's E2EE hello advertises `compression: ["deflate-raw"]`;
  the server answers with the one it will use, or nothing. Older peers on
  either side keep uncompressed records.
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
