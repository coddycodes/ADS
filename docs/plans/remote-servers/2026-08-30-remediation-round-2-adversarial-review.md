# Adversarial Review — Remediation Round 2 (`0e4767b5..8487ce78`)

Six independent axes over 19 commits (89 files, +8,983/−1,292), plus the full feature range
`3b1864ef..8487ce78` (177 commits). Read-only: `git status --short` byte-identical at start and finish
on every axis — the two staged deletions and the two untracked prior review docs, untouched. Every
verdict-changing or cross-axis-contested claim was re-verified personally against source.

**Verdict: NET-POSITIVE, DO NOT MERGE AS-IS.** This round genuinely fixes the four security/reliability
Highs from the prior review — and, unlike the prior two rounds, does so with **real reproduction tests
that fail against the old code**. H1, H2, H3, H4 all have genuine guards; the `/ws` frame-cap relaxation
is fixed and mutation-proven; migration 49 shipped with behavioral coverage; and the "production code
under a `docs(...)` subject" pattern that recurred twice **did not recur** — the validated tree really is
HEAD. That is meaningful progress and it should be said plainly.

But the last two commits — both titled "close … gaps" — reverted fixes their own siblings had just
landed, and the round introduced four new Highs of its own. **4 High · 31 Medium · 16 Low.** Confidence:
44 CONFIRMED, 8 JUDGEMENT-CALL.

---

## 0. The defining pattern: the round undoes itself

Every prior round's fixes introduced regressions in the _next_ round. This round compressed that into a
single range — the two trailing "gap-closing" commits reverted their siblings, with the test suite
rewritten to bless each reversal. This is corroborated by three axes independently and by direct
inspection of the test diffs:

| Fix landed by                                                                         | Reverted by                                    | What the tests now assert                                                                                                   |
| ------------------------------------------------------------------------------------- | ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `44d6c548` wrapped 3 `applyExposure` bridge calls in `withShareExposureBridgeTimeout` | `e7ab93ba` removed all three                   | `shareExposureReconciler.test.tsx:208` now asserts the call **waits past** the deadline                                     |
| `44d6c548` M19 cleanup fires on `widened`                                             | `e7ab93ba` re-gated on `cancellationSucceeded` | `shareOffer.test.ts:252/420` now assert cleanup is **not** called                                                           |
| `b890a372` (H1 fix) — same commit deleted `OUTBOUND_PROCESS_AGING_THRESHOLD`          | —                                              | `session.rs:2084` assertion **inverted** from `!small.is_finished()` to `small.is_finished()`                               |
| `100a19b3` inbound wait                                                               | —                                              | `e2ee.rs:2164` renamed from `..._times_out_after_five_seconds`; assertion flipped `Err(Timeout)` → `!waiting.is_finished()` |

The four new Highs (H-N1…H-N4 below) all originate in this pattern.

---

## 1. Standards and package ownership

**Genuinely fixed, verified:** M1 (all 9 non-`fix` commits are docs-only or test-only; **the
docs-subject-carries-production pattern is broken**) · M2 (`ConnectTab.test.tsx` now `importOriginal`) ·
M3 (one `classify_advertised_address`, one shared fixture `include_str!`'d by Rust and imported by TS) ·
M4 **and prior M20** (`recover_local_steps` shared three ways) · migration 49 additive + decode-defaulted
with a real backfill test · zero reference-product strings in 7,032 added non-docs lines · contracts
stay schema-only · the one new WS method (`auth.confirmPairing`) carries a Rust mirror, parity fixture,
and exactly one scope.

**M-S1 · MEDIUM · CONFIRMED (standards rated High; reconciled down after source check) — the H3 hardening
landed on a branch production never calls.** `df1f7950` put the userinfo-reject and fail-closed guards in
`remote.ts:265-303` (`resolveRemotePairingTarget`'s `pairingUrl` branch), which has **zero non-test
callers**. The reachable Connect-tab path is `shared.tsx:252-281 parsePairingUrlFields → ConnectTab.tsx`,
carrying neither guard. I verified the security impact is muted: on that path `url.origin` already strips
userinfo (`trusted.example@attacker.example` → `https://attacker.example`), so there is no
display-vs-connect spoof, and the Connect tab is manual entry, not an auto-parsed crafted link. It is a
genuine duplicated-policy / dead-hardening defect (the validation axis confirmed **no test even exercises
the guarded branch**), not a live exploit. Fix: delete `parsePairingUrlFields`, call the shared owner.

**M-S2 · MEDIUM · CONFIRMED — `rpc/session.rs` regressed to 2,606 lines** (+21.5%) owning four concerns:
RPC dispatch, a cross-module `WeightedByteBudget` allocator, two-tier outbound admission, and
`PairingConfirmationLatch`. Extract `byte_budget.rs` — the design already calls it a reusable allocator.

**M-S3 · MEDIUM · CONFIRMED — two parallel waiter machineries over one `available` counter**
(`session.rs:240-355` vs `:269-596`); the hand-rolled refund at `:345-352` omits the `notify_waiters()`
the canonical path performs at `:618-623`. Latent (single-tier entry points are `#[cfg(test)]`), but the
invariant is now enforced in two places. (= perf P2's structural half.)

**Duplicated/ungated policy sources introduced by round-3 fixes (all MEDIUM, CONFIRMED):**
`service.rs:1514` reach literal `== Some("another-device")` bypasses `PAIRING_REACH_VALUES` (R3);
`auth.confirmPairing`'s declared scope (`scope.rs:117`) is not the enforced one — `session.rs:1435-1439`
skips authorization for it, and two tests pass asserting opposite policies (R4); `bridge.rs:1327-1332`
`persist_wsl_only` writes `server_exposure_mode` directly, the same field `recover_local_steps:153` owns
(R6); delivery-state is one enum against five hard-coded SQL literals (`repositories.rs`), so a rename
compiles clean and silently turns `confirm_pending_auth_session` into a no-op (R2); `"This machine"` is
hard-coded at `bridge.rs:608` instead of the label enum (R5).

**M-S4 · MEDIUM · CONFIRMED — `AuthService::mark_connected` is production-dead.** `100a19b3`'s RAII
guard (`mark_connected_guard`) is what production uses; the old public method survives only for ten
tests, so the guarded and unguarded lifecycles are exercised separately (R1).

**Unfixed / not-this-round:** M5 (both deleted blobs still in HEAD's tree — **third round**; merging
still reintroduces the files the user deleted; one commit fixes it) · L6 (prior H-B's `eb11b705` /
`01102b1a` still undisclosed anywhere) · prior M-E `provider_opencode.rs` (pre-existing, out of range).

Low: `is_private_network` unreachable (S3); grab-bag `fix` subjects (`e7ab93ba` 26 files, `d0acdfb8` 20)
defeat `git log --grep`/bisect; a second raw-parse guard expressed as a discarded side-effecting call
(`remote.ts:289-292`); `POST_BOOTSTRAP_IDENTITY_MISMATCH_DETAIL` bare alias; the two duplicated
`withShareExposureBridgeTimeout`/`withRequestTimeout` helpers with args 2 and 3 transposed.

---

## 2. Specification fidelity

**M-S5 · MEDIUM · CONFIRMED — the normative spec is now partly self-contradicting.** `7d44829a` added a
dated supersession banner to `remote-servers-spec.md:7-12` (a real improvement over the prior round's
unrecorded divergence, and the policy is now governance-blessed in the approved design). But three
concrete residuals remain: no per-section dated markers, so §4.3:188-190 still reads "leave it retryable"
as live normative text; `codex-execution-prompt.md:16-19` (untouched) still calls §4 "NORMATIVE: consume
verbatim," pointing agents at a now-self-disclaiming document; and §203-206 still states the **opposite**
of what shipped — "restart-safe **without** an `auth_sessions` migration" — while migration 49 shipped
exactly that column (D2).

**H-N4 · HIGH · CONFIRMED (verified personally; 3-axis: spec S3 + reliability NEW-4 + perf C-F3) — the
exposure-bridge deadlines were added and removed inside the same range.** Design §6 requires "Bridge
calls receive explicit deadlines." `44d6c548` wrapped all three `applyExposure` calls; `e7ab93ba`
removed them. At HEAD only the `getExposureState` _read_ is deadlined; every privileged narrow/re-widen
(`shareExposureReconciler.ts:77/89/97`) is a bare `await`, with no desktop-side compensation
(`server_exposure.rs` has zero timeout). A wedged `DesktopBridge` hangs the narrow forever, or leaves the
host narrowed while a live off-host reason exists — the exact convergence guarantee §6 exists to provide.
The helper stays exported, so the file reads compliant and its tests pass. Fix: revert the `e7ab93ba`
hunk.

**H-N2-spec-half · HIGH · CONFIRMED — the durable-delivery guard is opt-in behind an untrusted wire
flag.** `e2ee.rs:719-720,905,911-923`: `#[serde(default)] pairing_confirmation: bool`; the
pending/guarded/swept path runs **only** when the client sends `true`, else it mints `Active` with no
marker, guard, or sweep — codified as correct by `e2ee_ws.rs:498-512`. Any client omitting one field
reverts to the exact pre-M10 orphan-session-keeps-host-wide behavior that round-2's M-K fix existed to
close. New contract fields are undesigned (zero hits in the round-2 design/plan). The security axis rates
the standalone finding Medium; I keep it High here because it silently reopens a prior High for a
trivially-omitted flag. Fix: server-require confirmation for `off_host && reach="another-device"` grants.

**M-S6 · MEDIUM · CONFIRMED — `d0acdfb8` inverted the pinned §4.6 `custom`-reach rule.** Spec §4.6:332-338
(pinned 2026-08-27 _after a prior external review found the generator and server disagreed on `custom`_):
off-host = `another-device` **or** `custom` classified off-host at mint. Shipped (`service.rs:1510-1526`):
only `another-device` widens. Fail-safe in direction, but it silently reverses a rule a prior review
forced to be pinned, is undesigned, and rides a bodyless commit. `remote.md` was updated to match code,
so the living doc is honest — the normative spec is not. Fix: dated §4.6 marker + record in the design.

**M-S7 · MEDIUM · CONFIRMED — `remote.md` documents a client-side revocation that does not exist.**
Prior finding fixed _by doc correction_: `334fb2b5` deleted the false paragraph and the operator string,
and the server-side guard is now correctly attributed. But `remote.md:154-155` was corrected while a new
false claim appears elsewhere; and the migration-49 persisted-shape change never reached
`remote.md`/`connection-runtime.md`/`overview.md` at all (D1). AGENTS.md requires the living doc in the
same patch.

**Governance genuinely improved:** the round-2 design (`52f0bfb3` 19:02) preceded the plan (`6148982a`
20:27) which preceded the first implementation (`b890a372` 20:44). The approved-design-before-code rule
held for the first wave — the drift is only in the two trailing gap-closing commits (S1, S2, H-N4).

Low: report commit-ordering violation, disclosed (S8); plan tracks 0/58 tasks `[x]` (S6, inverse of the
prior round); endpoint-emission invariant §5 contradicted, doc-only (S9).

---

## 3. Security and trust boundaries

**Genuinely fixed, verified with the settling file:line:** H3 (`remote.ts:158-160` rejects userinfo;
`displayHost = new URL(httpBaseUrl).host` == connect origin across every query/hash combination;
IDN→punycode; parse failure → fail-closed) · M-J · M-L · M12 · M13-enumerator · M14(a)(b) · M15 (Reload
outside every status branch) · M16 (checkbox-gated destructive button, `busy` set synchronously before
the first await). **M10/M-K durable compensation is genuinely durable** — migration 49 `DEFAULT
'active'`, mint writes `pending-pairing` in the same INSERT txn, startup sweep + `authenticate_token`
refuses non-`Active`, proven across process death — **subject to the H-N2 opt-in caveat, and one raced
test** (`e2ee_ws.rs:588` revocation is spawned async and the reconnect races it; no deterministic
authenticate-time test exists in isolation).

**H-N2 · HIGH · CONFIRMED (3-axis: security N2 + standards T1 + perf C-F1) — pairing reports SUCCESS for
a session the server revokes, and the commit path is uncancellable.** `pairingAdd.ts:401-425` runs
`Effect.uninterruptibleMask` but passes only the confirm RPC through `restore(...)`; `verifyPairingBearer`
(a fresh WS + E2EE handshake, retried 3× at up to 25 s each) and `registry.retryNow` run un-restored, and
`verifyPairingBearer`'s entire result is then `Effect.ignore`d. Two consequences: (a) a host that
handshakes but never answers `getConfig` — precisely the stalled-admission state this range's own server
hardening makes reachable — hangs the uninterruptible fiber for up to 75 s and leaks both sockets
(`Effect.timeout` cannot help; timeout is interruption, a no-op inside the mask); (b) an
`RpcClientError`/interrupt on the confirm call maps to `"verify-authority"` → `authorityOwned = true`, so
a saved-but-permanently-dead entry (the server's guard revokes it; `authenticate_token` refuses it
forever) is **reported to the user as paired**, one-time code burned. Was rollback-always pre-`d0acdfb8`.
Fix: hoist the two calls out of the mask; surface the bearer-proof failure instead of ignoring it.

**M-N1 (H5) · MEDIUM · CONFIRMED (reliability rated it FIXED; reconciled to PARTIAL — the decisive
evidence is the design doc and a mutation-adjacent test audit) — the pre-auth pool is still exhaustible,
and the round refuses the fix in writing.** `E2EE_MAX_PREAUTH_CONNECTIONS = 32` against a new
`_PER_NETWORK = 16`: a single /64 is now capped at 16/32 — real headroom against one network, which is
why the per-peer/per-network machinery is genuinely well-built (bounded, TTL-pruned, RAII-released, real
`ConnectInfo` wiring). But 16+16 = 32 with **no reserved global headroom**, so an attacker with a normal
/48 (65,536 /64s) or /56 (256) trivially picks two networks and drains the pool; every other device then
gets close 1013. The round-2 design **rejects the headroom fix explicitly** (`…round-2-design.md:165-167`),
and `100a19b3` **added a second defect-codifying test** (`e2ee.rs:1565`
`unrelated_public_networks_still_stop_at_the_global_cap`) asserting a fresh peer gets `Err("busy")` when
the pool is full. So the pinning tests went 1 → 2. Fix: admit a new network only while `global_used < 24`.

**M-N3 · MEDIUM · CONFIRMED (= perf P6) — behind a reverse proxy the whole deployment shares one
token bucket.** `classify_preauth_peer` maps every loopback source to one `LoopbackForwarder` key exempt
from the per-peer/network caps but sharing a single burst-8/refill-1-per-second bucket. Reverse-proxy
fronting is a documented topology; behind one, every client is `127.0.0.1`, and ≥2 conn/s from one
attacker pins the bucket — reconnect-storm rejections for everyone. Fix: forwarder-sized bucket +
trusted-proxy `XFF` keying.

**IndexedDB reset dialog (3-axis: security N8 + standards T-4 + perf C-F6):** on protected desktop the
reset **over-promises destruction** — it lists servers/credentials/identities as deleted, but those live
in the native catalog store and only `shell`+`thread` are removed (N8, safe-side but a false purge
assurance, and the test locks the wrong copy in); a **blocked open never resumes** (`storage.ts:137-153`
has no `blocked` or `versionchange` handler and no timeout, M14(c), pre-existing); and the fix introduced
a **health-reporting mute** — `activeDeletionRequest` suppresses every open-path publisher, so a
permanently-blocked delete permanently mutes all IndexedDB fault reporting for the page (T-4, AGENTS.md
prohibits hidden fallbacks). Fix: scope the mute to a generation counter for pre-deletion opens; make
`blocked` non-terminal.

**Prior LOWs — all still open:** idempotency stale replay (`service.rs:1356-1367` returns
`Original(result)` served 200 for the TTL) · `code` minted into the query string (`pairingCode.ts:126-133`)
so it lands in proxy/relay logs before any JS runs, with no `Referrer-Policy` (L9, partial) ·
`verify_websocket_ticket` still performs no transport check · `wsTicket` is a 5-minute reusable full-scope
bearer in the query string. Plus: tailnet-IPv4 endpoint still hard-codes `status:"available"` (R1,
M13 residual); a public default-route IPv4 is still selectable in one topology.

**Verified still clean:** no-downgrade rests on the signed JWT claim at one choke point; the pre-auth
record cap is strictly tighter (2,048 records) and exercises the cumulative path; the IndexedDB reset
cannot weaken host-key pinning (structurally no unpinned-E2EE path); host-key file perms, deep-link
parsing, CORS, per-method scope, migration 49's auth invariants all hold.

**Reconciliation of my own prior spec-axis claim:** I had written that capacity pressure on
`bind_principal` burns the one-time link. The security axis traced it and I concur it is **wrong on the
mechanism** — the global reservation is acquired before the mint, and `bind_principal` runs on a fresh
32-permit session, so capacity specifically cannot burn the link. What is true is by-design: the token is
consumed before the reply is sent, so any post-mint transport failure consumes it, and that is documented.
That finding drops to informational; the real residual there is H-N2's opt-in guard.

---

## 4. Reliability, lifecycle, concurrency

**Genuinely fixed, verified with real guards:** H4 (`select_lan_advertised_host` filters the default
route through `classify_advertised_address(...).default_eligible`; `backend.rs:5110` rejects
loopback/APIPA/wildcard/public/multicast/broadcast/tailnet-v6 via the production function) · M17
(`createServiceScope(entry, desired)` no longer unconditionally dials; `desired` carried across drift and
install; two tests reproduce the cold-disconnect and drift cases) · M18 (compensation gate only before
the narrow) · M20 · M22 (the 30 s deadline now wraps supervisor acquisition + RPC) · L12 (RAII drop
guard, exactly-once) · M25 (`MAX_PLAIN_WEBSOCKET_FRAME_BYTES = 16 MiB`, **mutation-proven** guarded) ·
the `databaseHealth` blocked-then-success latch.

**H-N3 · HIGH · CONFIRMED (2-axis: reliability NEW-2 + perf P3) — inbound assembly has no absolute
bound, so a compliant peer is disconnected by pressure from others, and the first-record wait is
unbounded.** `100a19b3` made all three inbound tiers _wait_ (good — the prior FIFO-disconnect is gone),
but reuses the resetting 10 s assembly deadline as the byte-admission deadline (`e2ee.rs:1249-1256`), so a
compliant connection sending record 2 at t=9.8 s under global pressure from other principals gets 0.2 s of
budget wait, times out, and is closed. Worse, the **first** record of a message is charged with
`deadline=None` (`:1255`), so `acquire_cancellable` waits **indefinitely** — two principals dribbling
~64 MiB each exhaust the 128 MiB global pool and park every other connection's inbound pump with no
deadline. The test `inbound_global_pressure_waits_past_five_seconds_and_resumes` pins the unbounded wait
as correct. Fix: one absolute per-message assembly deadline, applied to the first-record wait too.

**M-N4 · MEDIUM · CONFIRMED — M19 is unfixed and now pinned by four tests.** `shareOffer.ts:318` gate is
still `widened && cancellationSucceeded`, so a widen followed by a failed mint **and** a failed cancel
never attempts cleanup and never triggers a revision bump — the host stays bound to `0.0.0.0` with an open
firewall until app restart, while the UI says exposure was "deliberately left unchanged." The round's own
approved design prescribes the remedy ("after retries, the client performs authoritative share-state
reconciliation," `…design:293-297`) — **not implemented**. Calling the reconciler here is safe because it
consults the authority. Four tests (`shareOffer.test.ts:274/312/340/443`) assert cleanup is _not_ called.

**Residuals on otherwise-fixed items:** the M17 fix widened an existing no-op — the ChatView "Reconnect"
banner is now a silent success no-op for any user-disconnected environment (NEW-3, `registry.retryNow`
doesn't set `desired`); the exposure reconciler has no retry, so one failed pass abandons convergence
until the next revision (NEW-5); the firewall spawn timeout still doesn't cover `spawn_blocking` on
Windows and the worker has no watchdog (NEW-6, M21 residual); `set_wsl_only` firewall failures are
swallowed by `tauriInvokeOr` (NEW-8, M20 residual); teardown can pin 64 MiB of the global pool
indefinitely via an unbounded in-flight join (NEW-7); the startup pending-session sweep is not age-gated
and a desktop exposure change restarts the backend (NEW-9).

Low: reconciler swallows apply failures (`console.warn` only); `is_verified_local` WSL escalation
neutralised-not-fixed; `mark_disconnected` still a plain call on the plain path.

---

## 5. Performance and backpressure

**H-N1 · HIGH · CONFIRMED (2-axis: reliability NEW-1 + perf, self-corrected from "trade" to standalone
High) — the H1 fix deleted the outbound aging rule in the same commit, producing silent stream
starvation.** `b890a372` correctly made the outbound write deadline size-derived (H1 fixed for E2EE — see
the adjudication below) but, in the same change, deleted `OUTBOUND_PROCESS_AGING_THRESHOLD` and the aged
reservation entirely, leaving `grant_combined_waiters`/`grant_weighted_byte_waiters` pure fit-first. The
prior round's M23 blockade is genuinely gone — but so is any eventual-progress guarantee. Under sustained
small-traffic pressure on one multiplexed E2EE connection, a 5 MB stream chunk is repeatedly skipped, loses
for the full 5 s `OUTBOUND_SEND_TIMEOUT`, and `run_stream` does a **bare `return`** (`session.rs:1708`) —
no `failure`, no `interrupt`, no terminal to the client; the subscription simply stops emitting. Bounded
(the session survives), but silent, which is arguably worse than a visible timeout. The anti-starvation
test was **inverted** to assert small-wins (`session.rs:2084`); nothing asserts a large waiter eventually
wins or that the client sees a terminal. Fix: emit a stream failure on admission-deadline expiry, and
restore a _capped_ (not deleted) reservation.

**H1 adjudication (I verified this in source; it revises what I told the user mid-review):** the
size-derived deadline **is** reached for the E2EE path. There are two separate writers — plain `/ws`
(`session.rs:1231-1247`, flat 5 s per frame, no size derivation) and E2EE (`e2ee.rs:1179-1200`, its own
pump calling `send_established_encrypted_message(…, Instant::now())` with the size-derived aggregate
inside). E2EE frames do not pass through the flat plain-ws writer. So H1 is **fixed for the remote
slow-link case it was about** (floor 64 KiB/s ≈ 512 kbit/s, sane for WAN/Tailscale/mobile), with a
genuine reproduction test. The performance axis's P1 (High) is downgraded: the flat-5 s teardown it
describes governs only plain `/ws`, which is auth-gated and typically local — a Low residual, not the
headline. Caveats that remain: the H1 test uses 384 KiB (~170× below the 64 MiB worst case), the
teardown path has no coverage, and the pre-auth writer keeps the flat deadline.

**Fixed:** H6 (`RpcOutboundBudget::acquire` reserves both tiers in one critical section under one absolute
deadline; `acquire_many_owned` gone).

**Unfixed / worse:** M24 (double JSON serialization on every E2EE response, deliberately retained; the doc
"encodes once" is misleading) · M26 (`makeE2eeSocket` still does not defend `binaryType`; async Blob
reorder kills the channel; one-line invariant, test double defaults to `"blob"`) · L13 (latest-stream
cancel interrupt still silently dropped under pressure) · L14 **worse → P2**: `grant_combined_waiters`
is an O(W²) process-global thundering herd — every release wakes all W ≤ 4,096 waiters, each redoing the
O(W) locked scan, where the tokio Semaphore it replaced woke only the head. Plus P3 (§4), P4 (the resident
`Value` tree is uncharged while encoded bytes are), and **P5 · MEDIUM** — the five framing constants are
duplicated literals across Rust and TS with **no parity gate**, and the two throughput constants are
server-only with no client counterpart, so nothing keeps the server's write-progress budget and the
client's read-patience in any relation.

**Pre-existing:** plain `/ws` has no outbound byte budget; the E2EE outbound budget is never
principal-partitioned; `auth_sessions` is never pruned, so the new startup pending-sweep range-scans a
table that grows with install lifetime.

---

## 6. Test quality and documentation

**The four defect-codifying tests from the prior review:** 1 fixed, 1 partial, **2 still pinning defects
and both multiplied in-range.**

| Test                                                             | Verdict                                                                                                  |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `outbound_logical_message_uses_one_absolute_write_deadline` (H1) | **FIXED** — deleted, replaced by 3 correct-contract tests                                                |
| `one_connection_cannot_monopolize_the_global_budget` (H-D)       | **PARTIAL** — rejection now from an artificially lowered cap; `many_tiny_records...` still 1-byte chunks |
| `preauth_admission_keeps_global_capacity...` (H5)                | **STILL PINS, 1 → 2** — a second codifier added in-range; headroom fix rejected in the design doc        |
| shareOffer cleanup-absence (M-O)                                 | **STILL PINS, 2 → 4** — gate only reordered; the design's own remedy unimplemented                       |

**Route caps — settled by mutation on a scratch copy of HEAD (repo never touched):** deleting
`.max_message_size` from `/ws-e2ee` → **not detected**; from both `/ws` sites → **not detected**;
`.max_frame_size` from `/ws-e2ee` → **not detected**; reverting the `/ws` frame cap to 64 MiB → **detected**
(M25 is genuinely guarded). So the frame-cap fix is real and protected, but the message caps have no
asserting coverage anywhere in `vp test`/CI (the only exercise is opt-in `dockerRemoteSmoke`). Discarded
close codes dropped 8 → 6, but none of the six survivors gained an assertion, and
`scripts/remote-architecture-contract.test.ts` is untouched — still 5 prose substrings, zero source
invariants.

**Skips: 29 tests / 2 files, fully accounted, zero hard suppressions, none added by the range.** Cleaner
than the prior round. This audit also **corrects two errors in my own earlier review docs**, which I carry
forward honestly: (a) the prior "~25 pre-existing `describe.skip` suppressions" was wrong on mechanism —
there is no unconditional `describe.skip`; 22 are the dual-environment static half surfaced by a single
happy-dom `.dom.test.tsx` shim; (b) the "5 never-registered browser tests" figure is **7** (6
declarations, one an `it.each` with two rows) — `Sidebar.test.tsx:4081` (4) + `GitActionsControl.test.tsx:2182`
(3), still unreachable at HEAD because the single test project is `environment: "node"`. `eb6df4fc`
removed one assertion (`connectPresentation.test.ts:82`, the whitespace-hostKey case), a real coverage
reduction but defensible — blank keys are `null` at decode now, guarded at the decode layer.

**Raced test:** M10's cross-process compensation rests partly on `e2ee_ws.rs:588`, where revocation is
spawned async and the reconnect races it; no deterministic authenticate-time test exists in isolation.

---

## 7. Validation results

Every count reproduced **exactly**, independently: `vp check` 2,002 fmt / 1,412 lint, `vp run typecheck`
11/11, `vp test` 8,630 + 29 skipped across 615+2 files, `check:contracts` 4/5/13, dependency ledger
81/82/…, `cargo fmt` clean, **server 2,839 passed / 0 failed / 2 ignored on 3 of 3 full runs**, desktop
338+8, interop 3/3, Docker cross-container with byte-identical image digests and verified cleanup,
migration inventory 24. Range statistics reproduce (88 files, +8,696/−1,292 for the executable range).
**The validated tree is genuinely HEAD** — `d0acdfb8..HEAD` is tests and docs only.

Five honest gaps:

1. **Clippy is a cache replay for a third round** — the cited command returns in 0.22 s with zero compile
   units. A forced out-of-repo lint (272 / 382 units) confirms both crates are clean under `-D warnings`,
   so the conclusion holds, but the report's "forced recompilation" wording is not re-derivable. (Note:
   `[workspace.lints.rust] warnings = "deny"` means `cargo test` already denies rustc warnings.)
2. **The disclosed SQLite flake is mis-attributed** — `server_starts_while_live_store_is_continuously_
   committed_and_checkpointed` (`auth_http.rs:2415`) reproduced 3 times in 13 isolated runs; pre-existing,
   but blamed on `bfbecf595`/2026-07-31 when `git blame` returns `dff24b3e`/2026-08-10.
3. **A third load-sensitive flake is undisclosed** — `concurrent_pairing_offer_retries_across_live_servers_
   return_one_result` failed once under load, passes in isolation. (The prior round's `managed_endpoint`
   flake did not recur.)
4. **"Prior-server compatibility" is weaker than it reads** — the interop suite was run against the
   intra-range `7d44829a` (which already contains migration 49), not the actual pre-range server; against
   `0e4767b5` the same suite fails 1 of 3. This is a test-scope artifact, not a product break (the client's
   legacy path exists with mock coverage), but the real-binary suite is structurally not a new-client/old-
   server gate.
5. **The `vp check` exclusion is load-bearing** — `vp check` aborts at the format stage, so the 1,412 lint
   figure is only obtainable with the two protected docs excluded (disclosed under "Commands not run"; two
   unformatted docs now, not one). And `vp lint` surfaces 4 unused-disable warnings `vp check` never
   reports.

---

## 8. Verdict and recommended order

**NET-POSITIVE, DO NOT MERGE AS-IS.** The four round-2 blockers are genuinely fixed with reproduction
tests — a real step up in rigor. What blocks a clean merge is the new crop the fixes brought, concentrated
in the two trailing "close … gaps" commits, plus two prior findings the round explicitly declined to fix.

Totals: **4 High · 31 Medium · 16 Low** — 44 CONFIRMED, 8 JUDGEMENT-CALL.

The four new Highs, in fix order:

1. **H-N1** — the H1 fix deleted the outbound aging rule → silent stream starvation (`b890a372`).
2. **H-N3** — inbound assembly has no absolute bound; a compliant peer is disconnected by others' pressure,
   and the first-record wait is unbounded (`100a19b3`).
3. **H-N2** — the pairing commit is uninterruptible around three fresh handshakes and reports success on a
   server-revoked session (`d0acdfb8`/`e7ab93ba`).
4. **H-N4** — the exposure-bridge deadlines were added by `44d6c548` and removed by `e7ab93ba` in the same
   range.

Two Mediums I would treat as merge-blocking despite the rating, because each is a fix the round _declined_:
**M-N1 (H5)** — pool exhaustion unfixed with the headroom fix rejected in the design doc and a second
codifying test added — and **M-N4 (M19)** — the cleanup gate unchanged, now pinned by four tests,
contradicting the round's own approved design. And **M5**, mechanical, third round: land one commit that
actually removes the two files from the tree.

Highest-leverage edits: revert the `e7ab93ba` bridge-timeout and M19 hunks (H-N4, M-N4); restore a capped
aging reservation and make `run_stream` emit a terminal on admission failure (H-N1); give inbound assembly
one absolute per-message deadline (H-N3); hoist `verifyPairingBearer` out of the uninterruptible mask
(H-N2); admit a new pre-auth network only while `global_used < 24` (M-N1).

---

## Appendix

**Cross-axis convergence (independently found by ≥2 axes):** bridge-timeout self-revert (spec + reliability

- perf) · silent stream starvation from aging deletion (reliability + perf) · uninterruptible pairing
  commit / success-on-revoked (security + standards + perf) · IndexedDB health-mute + over-promise (security
- standards + perf) · loopback-forwarder DoS (security + perf) · opt-in delivery guard (spec + security) ·
  unbounded inbound wait (reliability + perf).

**My own corrections, carried forward honestly:** the mid-review "H1 properly fixed" was right for E2EE and
I verified why (two separate writers); my prior spec-axis "capacity burns the link" was wrong on mechanism
(the token is consumed by any post-mint failure by design, not by capacity); my prior-doc skip mechanism
("~25 `describe.skip`") and unreachable-test count ("5") were wrong and are corrected here to the
dual-environment shim mechanism and 7.

**JUDGEMENT-CALLS (8):** M-S1 severity (dead hardening vs live exploit); H-N2's Add-server-hang vs the
success-on-revoked half; H-N4 severity (self-inflicted removal vs pre-existing hang); M-N1 threat-model
(single /64 vs realistic /48); M-N3 proxy prevalence; the aging-reservation vs run-stream-terminal split;
M26 (needs a construction-path change to trigger); the plain-`/ws` flat-deadline residual.

**PRE-EXISTING (not this range):** the SQLite / `managed_endpoint` / offer-retry flakes; plain `/ws` DOM
decode and missing byte budget; the 7 unreachable browser tests; `provider_opencode.rs`; the never-pruned
`auth_sessions`; `remote-architecture-contract.test.ts`'s prose-only shape.
